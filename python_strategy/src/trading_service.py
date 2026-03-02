"""
ZeroMQ-based Trading Service for FxScalpBot

Topology:
  - PUB  (tcp://*:5556) → Rust SUB : Ticks + Account updates
  - REP  (tcp://*:5557) ← Rust REQ : Order requests (returns immediately with "pending")
  - PUSH (tcp://*:5558) → Rust PULL: Async execution results (background fills)

Replaces the old raw TCP socket implementation.
"""

import json
import time
import logging
import os
import threading
from concurrent.futures import ThreadPoolExecutor

import zmq
import MetaTrader5 as mt5
from mt5_connector import MT5Connector

# Setup logging
handler = logging.StreamHandler()
handler.setFormatter(logging.Formatter('%(asctime)s - %(levelname)s - %(message)s'))
logger = logging.getLogger(__name__)
logger.addHandler(handler)
logger.setLevel(logging.INFO)
logger.propagate = False


class TradingService:
    """
    ZeroMQ service that provides MT5 data and execution to the Rust core.
    """
    def __init__(self):
        self.connector = MT5Connector()
        self.running = False

        # ZeroMQ context and sockets
        self.zmq_ctx = zmq.Context()
        self.pub_socket = None   # PUB: ticks + account
        self.rep_socket = None   # REP: order requests
        self.push_socket = None  # PUSH: async execution results

        # Thread pool for async order execution
        self.executor = ThreadPoolExecutor(max_workers=4, thread_name_prefix="mt5-exec")

    def start(self):
        if not self.connector.connect():
            logger.error("Could not start TradingService: MT5 connection failed.")
            return

        # Bind ZMQ sockets
        self.pub_socket = self.zmq_ctx.socket(zmq.PUB)
        self.pub_socket.bind("tcp://*:5556")
        logger.info("ZMQ PUB bound on tcp://*:5556 (ticks/account)")

        self.rep_socket = self.zmq_ctx.socket(zmq.REP)
        self.rep_socket.bind("tcp://*:5557")
        logger.info("ZMQ REP bound on tcp://*:5557 (orders)")

        self.push_socket = self.zmq_ctx.socket(zmq.PUSH)
        self.push_socket.bind("tcp://*:5558")
        logger.info("ZMQ PUSH bound on tcp://*:5558 (async results)")

        self.running = True

        # Start tick streamer in background thread
        threading.Thread(
            target=self._tick_streamer_loop,
            name="TickStreamer",
            daemon=True
        ).start()

        # Start account updater in background thread
        threading.Thread(
            target=self._account_updater_loop,
            name="AccountUpdater",
            daemon=True
        ).start()

        logger.info("TradingService ready — waiting for Rust connections...")

        # Main thread: handle REQ/REP order requests
        self._order_handler_loop()

    def _order_handler_loop(self):
        """Main loop: handles incoming order requests from Rust via REP socket."""
        while self.running:
            try:
                # Wait for a request (blocking, with timeout for graceful shutdown)
                if self.rep_socket.poll(timeout=1000):  # 1s timeout
                    raw = self.rep_socket.recv_string()
                    try:
                        request = json.loads(raw)
                        req_id = request.get("req_id", "")
                        method = request.get("method", "")
                        params = request.get("params", {})

                        logger.info(f"Request [{req_id}]: {method}")
                        response = self._process_request(req_id, method, params)
                        if req_id:
                            response["req_id"] = req_id

                        self.rep_socket.send_string(json.dumps(response))

                    except Exception as e:
                        logger.error(f"Error processing request: {e}")
                        error_resp = {"status": "error", "message": str(e)}
                        self.rep_socket.send_string(json.dumps(error_resp))

            except zmq.ZMQError as e:
                if self.running:
                    logger.error(f"ZMQ REP error: {e}")
            except Exception as e:
                if self.running:
                    logger.error(f"Order handler error: {e}")

    def _process_request(self, req_id, method, params):
        if method == "get_account":
            return {"status": "ok", "data": self.connector.get_account_state()}

        elif method == "get_tick":
            symbol = params.get("symbol", "EURUSD")
            return {"status": "ok", "data": self.connector.get_last_tick(symbol)}

        elif method == "analyze_momentum":
            ticks = params.get("ticks", [])
            symbol = params.get("symbol", "UNKNOWN")
            from momentum_detector import detect_momentum
            result = detect_momentum(ticks, symbol=symbol)
            if not result.get("detected") and result.get("rejection_reasons"):
                reasons = ", ".join(result.get("rejection_reasons", []))
                logger.info(f"[{symbol}] Momentum NOT detected: {reasons}")
            return {"status": "ok", "data": result}

        elif method == "analyze_reversion":
            ticks = params.get("ticks", [])
            from reversion_detector import detect_reversion
            result = detect_reversion(ticks)
            return {"status": "ok", "data": result}

        elif method == "execute_order":
            # ASYNC: Submit to thread pool, return immediately with "pending"
            self.executor.submit(self._execute_order_async, req_id, params)
            return {"status": "pending", "message": "Order submitted for async execution"}

        elif method == "close_position":
            # ASYNC: Submit close to thread pool, return immediately
            self.executor.submit(self._close_position_async, req_id, params)
            return {"status": "pending", "message": "Close submitted for async execution"}

        elif method == "get_symbol_info":
            symbol = params.get("symbol")
            info = self.connector.get_symbol_info(symbol)
            if info:
                return {"status": "ok", "data": info}
            return {"status": "error", "message": "Symbol not found"}

        else:
            return {"status": "error", "message": "Unknown method"}

    # ─── Async Execution (ThreadPoolExecutor) ─────────────────────────

    def _execute_order_async(self, req_id, params):
        """Runs in a background thread. Executes order on MT5 then pushes result via PUSH socket."""
        try:
            symbol = params.get("symbol")
            order_type = params.get("type")
            volume = params.get("volume", 0.01)
            slippage = params.get("slippage", 10)

            # Delegate to connector (handles Iceberg automatically for volume > 1.0)
            result = self.connector.execute_order(symbol, order_type, volume, slippage)

            if result is None:
                self._push_result(req_id, "error", message=f"MT5 returned None for {symbol}")
                return

            if result.retcode != mt5.TRADE_RETCODE_DONE:
                self._push_result(req_id, "error", message=result.comment, code=result.retcode)
                return

            logger.info(f"Order executed: {order_type} {result.volume} {symbol} at {result.price}")
            self._push_result(req_id, "ok", data={
                "ticket": result.order,
                "price": result.price,
                "volume": result.volume,
                "retcode": result.retcode
            })

        except Exception as e:
            logger.error(f"Async execute_order error: {e}")
            self._push_result(req_id, "error", message=str(e))

    def _close_position_async(self, req_id, params):
        """Runs in a background thread. Closes position on MT5 then pushes result via PUSH socket."""
        try:
            symbol = params.get("symbol")
            volume = params.get("volume")
            order_type = params.get("type")
            ticket = params.get("ticket")

            result = self.connector.close_position(symbol, volume, order_type, ticket=ticket)

            if result is None:
                self._push_result(req_id, "error", message="MT5 returned None for close")
                return

            if result.retcode != mt5.TRADE_RETCODE_DONE:
                self._push_result(req_id, "error", message=result.comment, code=result.retcode)
                return

            logger.info(f"Position closed: {symbol} {volume} {order_type} at {result.price}")
            self._push_result(req_id, "ok", data={"price": result.price, "ticket": result.order})

        except Exception as e:
            logger.error(f"Async close_position error: {e}")
            self._push_result(req_id, "error", message=str(e))

    def _push_result(self, req_id, status, data=None, message=None, code=None):
        """Pushes an async execution result to Rust via the PUSH socket."""
        result = {"req_id": req_id, "status": status}
        if data is not None:
            result["data"] = data
        if message is not None:
            result["message"] = message
        if code is not None:
            result["code"] = code

        try:
            self.push_socket.send_string(json.dumps(result))
        except Exception as e:
            logger.error(f"Failed to push async result: {e}")

    # ─── Tick Streaming (PUB socket) ──────────────────────────────────

    def _tick_streamer_loop(self):
        """Periodically broadcasts latest ticks to Rust via PUB socket."""
        root_dir = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        config_path = os.path.join(root_dir, "config", "risk_limits.toml")

        try:
            import toml
            config = toml.load(config_path)
            symbols = config.get("market", {}).get("symbols", ["EURUSD"])
        except Exception as e:
            logger.error(f"Failed to load symbols from config: {e}. Using default.")
            symbols = ["EURUSD"]

        last_tick_time = {symbol: 0 for symbol in symbols}

        while self.running:
            for symbol in symbols:
                tick = self.connector.get_last_tick(symbol)
                if tick:
                    if tick["time_msc"] > last_tick_time.get(symbol, 0):
                        last_tick_time[symbol] = tick["time_msc"]
                        message = json.dumps({"type": "tick", "data": tick})
                        try:
                            self.pub_socket.send_string(message)
                        except Exception as e:
                            logger.error(f"PUB tick error: {e}")
            time.sleep(0.02)  # 50Hz polling

    def _account_updater_loop(self):
        """Periodically broadcasts account state to Rust via PUB socket."""
        while self.running:
            try:
                account = self.connector.get_account_state()
                if account:
                    message = json.dumps({"type": "account", "data": account})
                    self.pub_socket.send_string(message)
            except Exception as e:
                logger.error(f"PUB account error: {e}")
            time.sleep(1.0)  # 1Hz update

    def stop(self):
        self.running = False
        self.executor.shutdown(wait=False)
        if self.pub_socket:
            self.pub_socket.close()
        if self.rep_socket:
            self.rep_socket.close()
        if self.push_socket:
            self.push_socket.close()
        self.zmq_ctx.term()
        self.connector.disconnect()


if __name__ == "__main__":
    service = TradingService()
    try:
        service.start()
    except KeyboardInterrupt:
        service.stop()

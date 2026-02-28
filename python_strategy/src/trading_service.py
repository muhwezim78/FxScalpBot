import socket
import json
import threading
import time
import logging
from mt5_connector import MT5Connector
import os
import MetaTrader5 as mt5

# Setup logging with flushing
handler = logging.StreamHandler()
handler.setFormatter(logging.Formatter('%(asctime)s - %(levelname)s - %(message)s'))
logger = logging.getLogger(__name__)
logger.addHandler(handler)
logger.setLevel(logging.INFO)
logger.propagate = False # Prevent duplicate logs

class TradingService:
    """
    Local TCP service that provides MT5 data and execution to the Rust core.
    """
    def __init__(self, host='127.0.0.1', port=5555):
        self.host = host
        self.port = port
        self.connector = MT5Connector()
        self.running = False
        self.server_socket = None
        self.clients = []
        self.clients_lock = threading.Lock()

    def start(self):
        if not self.connector.connect():
            logger.error("Could not start TradingService: MT5 connection failed.")
            return

        self.server_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.server_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.server_socket.bind((self.host, self.port))
        self.server_socket.listen(5)
        self.running = True
        
        logger.info(f"TradingService listening on {self.host}:{self.port}")
        
        # Start tick streamer thread
        threading.Thread(target=self._tick_streamer_loop, name="TickStreamer", daemon=True).start()
        
        while self.running:
            try:
                logger.info("Waiting for next connection...")
                client_sock, addr = self.server_socket.accept()
                logger.info(f"Accepted connection from {addr}")
                client_sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                t = threading.Thread(target=self._handle_client, args=(client_sock,), name=f"ClientHandler-{addr}", daemon=True)
                t.start()
            except Exception as e:
                if self.running:
                    logger.error(f"Error accepting connection: {e}")

    def _handle_client(self, client_sock):
        """Handles requests from Rust core using line-buffered socket."""
        try:
            addr = client_sock.getpeername()
            logger.info(f"Starting client handler for {addr}")
        except:
            addr = "unknown"

        with self.clients_lock:
            self.clients.append(client_sock)
        
        try:
            # Use makefile for line-buffered reading
            with client_sock.makefile('r', encoding='utf-8') as reader:
                logger.info(f"Reader established for {addr}")
                for line in reader:
                    line = line.strip()
                    if not line:
                        continue
                    
                    # Ignore HTTP noise or other non-JSON probes
                    if not line.startswith("{"):
                        logger.warning(f"Non-JSON line ignored: {line[:50]}")
                        continue

                    try:
                        request = json.loads(line)
                        req_id = request.get("req_id")
                        method = request.get("method")
                        params = request.get("params", {})
                        
                        logger.info(f"Request [{req_id}]: {method}")
                        response = self._process_request(method, params)
                        if req_id:
                            response["req_id"] = req_id
                        client_sock.sendall((json.dumps(response) + "\n").encode('utf-8'))
                    except Exception as e:
                        logger.error(f"Error processing {method if 'method' in locals() else 'request'}: {e}")
                        error_resp = {"status": "error", "message": str(e)}
                        if 'req_id' in locals() and req_id:
                            error_resp["req_id"] = req_id
                        client_sock.sendall((json.dumps(error_resp) + "\n").encode('utf-8'))
            
            logger.info(f"Client {addr} closed connection")
        except Exception as e:
            logger.error(f"Client handler error for {addr}: {e}")
        finally:
            with self.clients_lock:
                if client_sock in self.clients:
                    self.clients.remove(client_sock)
            client_sock.close()
            logger.info(f"Connection closed for {addr}")

    def _process_request(self, method, params):
        if method == "get_account":
            return {"status": "ok", "data": self.connector.get_account_state()}
        elif method == "get_tick":
            symbol = params.get("symbol", "EURUSD")
            return {"status": "ok", "data": self.connector.get_last_tick(symbol)}
        elif method == "analyze_momentum":
            ticks = params.get("ticks", [])
            from momentum_detector import detect_momentum
            result = detect_momentum(ticks)
            # Log rejection reasons if signal not detected
            if not result.get("detected") and result.get("rejection_reasons"):
                symbol = params.get("symbol", "UNKNOWN")
                reasons = ", ".join(result.get("rejection_reasons", []))
                logger.info(f"[{symbol}] Momentum NOT detected: {reasons}")
            return {"status": "ok", "data": result}
        elif method == "analyze_reversion":
            ticks = params.get("ticks", [])
            from reversion_detector import detect_reversion
            result = detect_reversion(ticks)
            return {"status": "ok", "data": result}
        elif method == "execute_order":
            symbol = params.get("symbol")
            order_type = params.get("type") # "buy" or "sell"
            volume = params.get("volume", 0.01)
            slippage = params.get("slippage", 10)
            sl = params.get("sl", 0.0)
            tp = params.get("tp", 0.0)
            
            # Prepare request structure
            mt5_type = mt5.ORDER_TYPE_BUY if order_type.lower() == "buy" else mt5.ORDER_TYPE_SELL
            tick = mt5.symbol_info_tick(symbol)
            if not tick:
                return {"status": "error", "message": f"Could not get tick for {symbol}"}
            
            price = tick.ask if mt5_type == mt5.ORDER_TYPE_BUY else tick.bid

            # Fetch symbol metadata for strict compliance
            info = self.connector.get_symbol_info(symbol) if hasattr(self, 'connector') else None
            
            vol_step = info.get("volume_step", 0.01) if info else 0.01
            vol_min = info.get("volume_min", 0.01) if info else 0.01
            volume_val = float(volume)
            volume_val = round(volume_val / vol_step) * vol_step
            volume_val = max(volume_val, vol_min)
            volume_val = round(volume_val, 2)
            
            print(f"Executing {order_type} {volume_val} {symbol} at {price}")

            request = {
                "action": mt5.TRADE_ACTION_DEAL,
                "symbol": symbol,
                "volume": volume_val,
                "type": mt5_type,
                "price": price,
                "sl": 0.0, # Pure Stealth: Broker sees no stops
                "tp": 0.0, # Pure Stealth: Broker sees no targets
                "deviation": int(slippage),
                "magic": 123456,
                "comment": "FxScalpBot Stealth",
                "type_time": mt5.ORDER_TIME_GTC,
                "type_filling": mt5.ORDER_FILLING_IOC,
            }

            # Enforce stops logic
            if info:
                digits = info["digits"]
                point = info["point"]
                stops_level = info["stops_level"]
                min_dist = stops_level * point
                
                # Pure Stealth: We no longer send SL/TP to MT5
                # The Rust core manages these targets internally
                request["sl"] = 0.0
                request["tp"] = 0.0

            result = mt5.order_send(request)
            if result is None:
                logger.error(f"Order failed: MT5 returned None for {symbol}")
                return {"status": "error", "message": "MT5 returned None", "code": -1}

            if result.retcode != mt5.TRADE_RETCODE_DONE:
                logger.error(f"Order failed: {result.comment} (code: {result.retcode})")
                return {"status": "error", "message": result.comment, "code": result.retcode}
            
            logger.info(f"Order executed: {order_type} {result.volume} {symbol} at {result.price}")
            return {
                "status": "ok", 
                "data": {
                    "ticket": result.order,
                    "price": result.price,
                    "volume": result.volume,
                    "retcode": result.retcode
                }
            }
        elif method == "close_position":
            symbol = params.get("symbol")
            volume = params.get("volume")
            order_type = params.get("type")
            ticket = params.get("ticket")
            
            result = self.connector.close_position(symbol, volume, order_type, ticket=ticket)
            
            if result is None:
                logger.error(f"Close failed: MT5 returned None for {symbol}")
                return {"status": "error", "message": "MT5 returned None", "code": -1}

            if result.retcode != mt5.TRADE_RETCODE_DONE:
                 logger.error(f"Close failed: {result.comment} (code: {result.retcode})")
                 return {"status": "error", "message": result.comment, "code": result.retcode}
            
            logger.info(f"Position closed: {symbol} {volume} {order_type} at {result.price}")
            return {"status": "ok", "data": {"price": result.price, "ticket": result.order}}
            
        elif method == "get_symbol_info":
            symbol = params.get("symbol")
            info = self.connector.get_symbol_info(symbol)
            if info:
                return {"status": "ok", "data": info}
            return {"status": "error", "message": "Symbol not found"}
        else:
            return {"status": "error", "message": "Unknown method"}

    def _tick_streamer_loop(self):
        """Periodically broadcasts latest ticks to all connected clients."""
        # Load symbols from config
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
                    # Deduplication: Only send if timestamp has advanced
                    # This prevents the Rust side from seeing 'stale' data aging
                    if tick["time_msc"] > last_tick_time.get(symbol, 0):
                        last_tick_time[symbol] = tick["time_msc"]
                        message = json.dumps({"type": "tick", "data": tick}) + "\n"
                        # Broadcast to all clients
                        with self.clients_lock:
                            current_clients = list(self.clients)
                        
                        for client in current_clients:
                            try:
                                client.sendall(message.encode('utf-8'))
                            except:
                                with self.clients_lock:
                                    if client in self.clients:
                                        self.clients.remove(client)
            time.sleep(0.02) # 50Hz polling for real-time responsiveness

    def stop(self):
        self.running = False
        if self.server_socket:
            self.server_socket.close()
        self.connector.disconnect()

if __name__ == "__main__":
    service = TradingService()
    try:
        service.start()
    except KeyboardInterrupt:
        service.stop()

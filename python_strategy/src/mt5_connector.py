import os
import logging
import MetaTrader5 as mt5
from dotenv import load_dotenv

# Setup logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

class MT5Connector:
    """
    Handles connection to MetaTrader 5 terminal and basic account operations.
    """
    def __init__(self):
        # Look for .env in the project root (two levels up from this script)
        base_dir = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        env_path = os.path.join(base_dir, ".env")
        load_dotenv(dotenv_path=env_path)
        
        # Also try local .env in case it's there
        load_dotenv() 
        
        self.login = os.getenv("MT5_LOGIN")
        self.password = os.getenv("MT5_PASSWORD")
        self.server = os.getenv("MT5_SERVER")
        self.path = os.getenv("MT5_PATH")
        self.connected = False
        self._symbol_cache = {} # Cache for symbol decimals/points

    def connect(self):
        """Initializes and logs into the MT5 terminal."""
        logger.info(f"Attempting to initialize MT5. Configured path: {self.path}")
        
        init_params = {}
        if self.path:
            if os.path.exists(self.path):
                init_params['path'] = self.path
                logger.info(f"Using custom MT5 path: {self.path}")
            else:
                logger.warning(f"Configured MT5_PATH does not exist: {self.path}. Falling back to default search.")
        
        if not mt5.initialize(**init_params):
            logger.error(f"initialize() failed, error code = {mt5.last_error()}")
            return False

        # If credentials are provided, attempt login
        if self.login and self.password and self.server:
            authorized = mt5.login(
                login=int(self.login),
                password=self.password,
                server=self.server
            )
            if not authorized:
                logger.error(f"Failed to connect to account #{self.login}, error code: {mt5.last_error()}")
                mt5.shutdown()
                return False
        
        account_info = mt5.account_info()
        if account_info is None:
            logger.error(f"Failed to get account info, error code: {mt5.last_error()}")
            mt5.shutdown()
            return False

        logger.info(f"Connected to MT5: {account_info.company} ({account_info.server})")
        logger.info(f"Account: #{account_info.login}, Asset: {account_info.balance} {account_info.currency}")
        
        self.connected = True
        return True

    def get_account_state(self):
        """Returns balance and equity for Rust core sync."""
        if not self.connected:
            return None
        
        info = mt5.account_info()
        if info:
            return {
                "balance": info.balance,
                "equity": info.equity,
                "margin": info.margin,
                "free_margin": info.margin_free,
                "profit": info.profit,
                "positions_count": mt5.positions_total()
            }
        return None

    def get_last_tick(self, symbol):
        """Fetches the latest tick for a symbol."""
        if not self.connected:
            return None
        
        tick = mt5.symbol_info_tick(symbol)
        if tick:
            # Use cache for static metadata to avoid redundant lookups every tick
            if symbol not in self._symbol_cache:
                info = self.get_symbol_info(symbol)
                if info:
                    self._symbol_cache[symbol] = info
            
            info = self._symbol_cache.get(symbol)
            return {
                "symbol": symbol,
                "bid": tick.bid,
                "ask": tick.ask,
                "last": tick.last,
                "volume": tick.volume,
                "time_msc": tick.time_msc,
                "digits": info["digits"] if info else 5,
                "point": info["point"] if info else 0.00001,
                "tick_value": info["tick_value"] if info else 1.0
            }
        return None

    def get_symbol_info(self, symbol):
        """Fetches symbol metadata like digits and points."""
        if not self.connected:
            return None
        
        info = mt5.symbol_info(symbol)
        if info:
            return {
                "digits": getattr(info, "digits", 5),
                "point": getattr(info, "point", 0.00001),
                "stops_level": getattr(info, "trade_stops_level", 0),
                "tick_size": getattr(info, "trade_tick_size", 0.01),
                "tick_value": getattr(info, "trade_tick_value", 1.0),
                "volume_min": getattr(info, "volume_min", 0.01),
                "volume_step": getattr(info, "volume_step", 0.01)
            }
        return None

    def close_position(self, symbol, volume, order_type, price=None, ticket=None):
        """Closes an active position. Improved version with auto-ticket lookup."""
        if not self.connected:
            logger.error("MT5 not connected, cannot close position")
            return None
        
        # Determine opposite order type
        mt5_type = mt5.ORDER_TYPE_SELL if order_type.lower() == "buy" else mt5.ORDER_TYPE_BUY
        
        # 1. If ticket is missing or zero, find it by symbol
        if ticket is None or ticket == 0:
            positions = mt5.positions_get(symbol=symbol)
            if positions is None or len(positions) == 0:
                logger.warning(f"No active position found for {symbol} to close.")
                return None
            # For simplicity, we take the first one. In a more complex bot, we'd match volume.
            ticket = positions[0].ticket
            logger.info(f"Auto-discovered ticket {ticket} for {symbol} closure.")

        # 2. Get current price if not provided
        if price is None:
            tick = mt5.symbol_info_tick(symbol)
            if tick:
                price = tick.bid if mt5_type == mt5.ORDER_TYPE_SELL else tick.ask
            else:
                logger.error(f"Failed to get current price for {symbol}")
                return None

        request = {
            "action": mt5.TRADE_ACTION_DEAL,
            "symbol": symbol,
            "volume": float(volume),
            "type": mt5_type,
            "position": ticket,
            "price": price,
            "deviation": 20, # Be slightly more generous on exit
            "magic": 123456,
            "comment": "Bot Exit",
            "type_time": mt5.ORDER_TIME_GTC,
            "type_filling": mt5.ORDER_FILLING_IOC,
        }

        result = mt5.order_send(request)
        if result is None:
            error = mt5.last_error()
            logger.error(f"mt5.order_send returned None for close. Error: {error}")
        return result

    def execute_order(self, symbol, order_type, volume, slippage=10):
        """
        Smart order execution. Uses iceberg strategy for large volumes.
        Returns a dict with ticket(s), price, volume, and retcode.
        """
        if not self.connected:
            logger.error("MT5 not connected, cannot execute order")
            return None

        info = self.get_symbol_info(symbol)
        vol_step = info.get("volume_step", 0.01) if info else 0.01
        vol_min = info.get("volume_min", 0.01) if info else 0.01

        # Normalize volume
        volume = float(volume)
        volume = round(volume / vol_step) * vol_step
        volume = max(volume, vol_min)
        volume = round(volume, 2)

        if volume > 1.0:
            return self._execute_iceberg(symbol, order_type, volume, slippage, info, vol_step, vol_min)
        else:
            return self._execute_single(symbol, order_type, volume, slippage, info)

    def _execute_single(self, symbol, order_type, volume, slippage, info):
        """Execute a single market order."""
        mt5_type = mt5.ORDER_TYPE_BUY if order_type.lower() == "buy" else mt5.ORDER_TYPE_SELL
        tick = mt5.symbol_info_tick(symbol)
        if not tick:
            return None

        price = tick.ask if mt5_type == mt5.ORDER_TYPE_BUY else tick.bid

        request = {
            "action": mt5.TRADE_ACTION_DEAL,
            "symbol": symbol,
            "volume": volume,
            "type": mt5_type,
            "price": price,
            "sl": 0.0,
            "tp": 0.0,
            "deviation": int(slippage),
            "magic": 123456,
            "comment": "FxScalpBot Stealth",
            "type_time": mt5.ORDER_TIME_GTC,
            "type_filling": mt5.ORDER_FILLING_IOC,
        }

        result = mt5.order_send(request)
        return result

    def _execute_iceberg(self, symbol, order_type, total_volume, slippage, info, vol_step, vol_min):
        """
        Iceberg execution: splits a large order into smaller chunks
        to reduce market impact and improve average fill.
        """
        CHUNKS = 5
        chunk_volume = round(total_volume / CHUNKS / vol_step) * vol_step
        chunk_volume = max(chunk_volume, vol_min)
        chunk_volume = round(chunk_volume, 2)

        mt5_type = mt5.ORDER_TYPE_BUY if order_type.lower() == "buy" else mt5.ORDER_TYPE_SELL

        tickets = []
        prices = []
        total_filled = 0.0

        logger.info(f"ICEBERG: Splitting {total_volume} into {CHUNKS} x {chunk_volume} for {symbol}")

        for i in range(CHUNKS):
            remaining = round(total_volume - total_filled, 2)
            if remaining <= 0:
                break

            this_volume = min(chunk_volume, remaining)
            this_volume = round(this_volume / vol_step) * vol_step
            this_volume = max(this_volume, vol_min)
            this_volume = round(this_volume, 2)

            tick = mt5.symbol_info_tick(symbol)
            if not tick:
                logger.error(f"ICEBERG chunk {i+1}: No tick for {symbol}")
                break

            price = tick.ask if mt5_type == mt5.ORDER_TYPE_BUY else tick.bid

            request = {
                "action": mt5.TRADE_ACTION_DEAL,
                "symbol": symbol,
                "volume": this_volume,
                "type": mt5_type,
                "price": price,
                "sl": 0.0,
                "tp": 0.0,
                "deviation": int(slippage),
                "magic": 123456,
                "comment": f"FxScalpBot Ice {i+1}/{CHUNKS}",
                "type_time": mt5.ORDER_TIME_GTC,
                "type_filling": mt5.ORDER_FILLING_IOC,
            }

            result = mt5.order_send(request)

            if result is None or result.retcode != mt5.TRADE_RETCODE_DONE:
                comment = result.comment if result else "None"
                logger.warning(f"ICEBERG chunk {i+1} failed: {comment}. Stopping split.")
                break

            tickets.append(result.order)
            prices.append(result.price)
            total_filled += result.volume
            logger.info(f"ICEBERG chunk {i+1}: filled {result.volume} at {result.price} (ticket={result.order})")

        if not tickets:
            logger.error(f"ICEBERG: All chunks failed for {symbol}")
            return None

        # Return a synthetic result-like object
        avg_price = sum(prices) / len(prices) if prices else 0.0
        logger.info(f"ICEBERG complete: {len(tickets)} fills, avg={avg_price:.5f}, total={total_filled}")

        # Return the FIRST ticket as the primary (Rust uses this for tracking)
        # All tickets are logged for manual reconciliation
        class IcebergResult:
            def __init__(self):
                self.retcode = mt5.TRADE_RETCODE_DONE
                self.order = tickets[0]     # Primary ticket
                self.price = avg_price       # Average fill price
                self.volume = total_filled
                self.comment = f"Iceberg {len(tickets)} fills"
                self.all_tickets = tickets   # All tickets for reference

        return IcebergResult()

    def disconnect(self):
        """Shutdown connection."""
        mt5.shutdown()
        self.connected = False
        logger.info("MT5 connection closed.")

if __name__ == "__main__":
    # Test connection
    connector = MT5Connector()
    if connector.connect():
        print(f"Account State: {connector.get_account_state()}")
        print(f"Tick EURUSD: {connector.get_last_tick('EURUSD')}")
        connector.disconnect()

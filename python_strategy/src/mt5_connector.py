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
        if not mt5.initialize(path=self.path if self.path else None):
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
        
        # 1. If ticket is missing, find it by symbol
        if ticket is None:
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

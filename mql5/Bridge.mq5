//+------------------------------------------------------------------+
//| Bridge.mq5 — thin TCP bridge EA (Phase 0 feasibility spike)      |
//|                                                                  |
//| The ONLY non-Rust piece of the system. Contains no strategy      |
//| logic. It:                                                       |
//|   * connects as a TCP client to the Rust engine (default         |
//|     127.0.0.1:5555) and reports broker/symbol specs on hello     |
//|   * streams ticks (NDJSON, one JSON object per line)             |
//|   * executes engine commands: order / modify / close / close_all |
//|   * validates every order (lots, mandatory SL, stops level)      |
//|   * is idempotent on order_id (duplicates get the stored reply)  |
//|   * sends account + position snapshots after every trade event   |
//|     and on every (re)connect for state reconciliation            |
//|                                                                  |
//| Protocol: README section 4. Engine-side extensions used by the   |
//| Phase 0 server: hello carries extra stops_level_pts; modify      |
//| failures are reported as order_reject with order_id              |
//| "modify-<ticket>"; close acks are fills with order_id            |
//| "close-<ticket>".                                                |
//+------------------------------------------------------------------+
#property copyright "filthi-bot"
#property version   "0.1.0"
#property strict

#include <Trade/Trade.mqh>

input string InpHost        = "127.0.0.1"; // engine host
input int    InpPort        = 5555;        // engine port
input int    InpTimerMs     = 100;         // read/heartbeat loop period
input int    InpReconnectMs = 1500;        // retry delay between connects
input long   InpMagic       = 424242;      // our magic (filters close_all/snapshot)
input string InpComment     = "xau-bridge-0";

CTrade g_trade;
int    g_sock          = INVALID_HANDLE;
bool   g_connected     = false;
long   g_out_seq       = 0;
long   g_last_hb_ms    = 0;
long   g_last_try_ms   = 0;
long   g_last_rx_ms    = 0;
long   g_hb_in         = 0;
string g_rx            = "";
string g_ids[128];     // idempotency ring: order ids
string g_replies[128]; // idempotency ring: stored replies
int    g_ring_next     = 0;

//+------------------------------------------------------------------+
//| Lifecycle                                                        |
//+------------------------------------------------------------------+
int OnInit()
{
   Print("[bridge] init host=", InpHost, " port=", InpPort, " magic=", InpMagic,
         " symbol=", _Symbol);
   if(!MQLInfoInteger(MQL_TRADE_ALLOWED))
      Print("[bridge] WARNING: algo trading not allowed for this EA yet ",
            "(check 'Allow Algo Trading' in EA properties and the global Algo Trading button)");
   for(int i = 0; i < ArraySize(g_ids); i++) { g_ids[i] = ""; g_replies[i] = ""; }
   EventSetMillisecondTimer(InpTimerMs);
   return(INIT_SUCCEEDED);
}

void OnDeinit(const int reason)
{
   EventKillTimer();
   Disconnect("deinit " + IntegerToString(reason));
}

//+------------------------------------------------------------------+
//| Tick streaming                                                   |
//+------------------------------------------------------------------+
void OnTick()
{
   if(!g_connected)
      return;
   MqlTick tk;
   if(!SymbolInfoTick(_Symbol, tk) || tk.bid <= 0.0 || tk.ask <= 0.0)
      return;
   string msg = "{\"type\":\"tick\",\"seq\":" + NextSeq() +
                ",\"t\":" + IntegerToString((long)tk.time_msc) +
                ",\"bid\":" + Dbl(tk.bid, _Digits) +
                ",\"ask\":" + Dbl(tk.ask, _Digits) + "}";
   Send(msg);
}

//+------------------------------------------------------------------+
//| Timer: connect/reconnect, read inbound, heartbeat outbound       |
//+------------------------------------------------------------------+
void OnTimer()
{
   long now = (long)GetTickCount64();

   if(!g_connected)
   {
      if(now - g_last_try_ms >= InpReconnectMs)
         TryConnect();
      return;
   }

   ReadInbound();
   if(!g_connected)
      return;

   if(now - g_last_hb_ms >= 1000)
   {
      g_last_hb_ms = now;
      Send("{\"type\":\"heartbeat\",\"seq\":" + NextSeq() +
           ",\"t\":" + IntegerToString((long)TimeGMT() * 1000) + "}");
   }
}

//+------------------------------------------------------------------+
//| Connection management                                            |
//+------------------------------------------------------------------+
void TryConnect()
{
   g_last_try_ms = (long)GetTickCount64();
   ResetLastError();
   int s = SocketCreate();
   if(s == INVALID_HANDLE)
   {
      Print("[bridge] SocketCreate failed err=", GetLastError());
      return;
   }
   if(!SocketConnect(s, InpHost, InpPort, 3000))
   {
      int err = GetLastError();
      Print("[bridge] SocketConnect failed host=", InpHost, " port=", InpPort, " err=", err);
      if(err == 4014)
         Print("[bridge] err 4014: address not in the allowed list. MT5 Options > ",
               "Expert Advisors > Allow WebRequest for listed URL: add '", InpHost, "'");
      SocketClose(s);
      return;
   }
   g_sock = s;
   g_connected = true;
   g_rx = "";
   Print("[bridge] connected to ", InpHost, ":", InpPort);
   SendHelloAndSnapshot();
}

void Disconnect(const string reason)
{
   if(g_sock != INVALID_HANDLE)
      SocketClose(g_sock);
   g_sock = INVALID_HANDLE;
   if(g_connected)
      Print("[bridge] disconnected: ", reason);
   g_connected = false;
   g_last_try_ms = (long)GetTickCount64() - InpReconnectMs; // retry on next timer
}

//+------------------------------------------------------------------+
//| Inbound                                                          |
//+------------------------------------------------------------------+
void ReadInbound()
{
   if(g_sock == INVALID_HANDLE)
      return;
   uint avail = SocketIsReadable(g_sock);
   if(avail == 0)
      return;
   uchar buf[];
   ArrayResize(buf, (int)avail);
   ResetLastError();
   int r = SocketRead(g_sock, buf, avail, 50);
   if(r <= 0)
   {
      Print("[bridge] SocketRead failed r=", r, " err=", GetLastError());
      Disconnect("read failed");
      return;
   }
   g_rx += CharArrayToString(buf, 0, r, CP_UTF8);
   int nl;
   while((nl = StringFind(g_rx, "\n")) >= 0)
   {
      string line = StringSubstr(g_rx, 0, nl);
      g_rx = StringSubstr(g_rx, nl + 1);
      if(StringLen(line) > 0)
         HandleLine(line);
      if(!g_connected)
         return;
   }
   if(StringLen(g_rx) > 1048576)
      Disconnect("rx buffer overflow");
}

void HandleLine(const string line)
{
   g_last_rx_ms = (long)GetTickCount64();
   string keys[], vals[];
   int rc = JsonParseFlat(line, keys, vals);
   if(rc != 0)
   {
      Print("[bridge] bad json (rc=", rc, "): ", line);
      return;
   }
   string type = JVal(keys, vals, "type");
   if(type == "heartbeat")
   {
      g_hb_in++;
      return;
   }
   if(type == "order")
      HandleOrder(keys, vals);
   else if(type == "modify")
      HandleModify(keys, vals);
   else if(type == "close")
      HandleClose(keys, vals);
   else if(type == "close_all")
      HandleCloseAll(keys, vals);
   else
      Print("[bridge] unknown message type: ", type);
}

//+------------------------------------------------------------------+
//| Commands                                                         |
//+------------------------------------------------------------------+
void HandleOrder(const string &keys[], const string &vals[])
{
   string oid = JVal(keys, vals, "order_id");
   if(oid == "")
   {
      Print("[bridge] order without order_id -> ignored");
      return;
   }
   // Idempotency: a re-sent order_id gets the stored reply re-sent.
   for(int i = 0; i < ArraySize(g_ids); i++)
   {
      if(g_ids[i] == oid)
      {
         Print("[bridge] duplicate order_id ", oid, " -> resending stored reply");
         Send(g_replies[i]);
         return;
      }
   }
   string side  = JVal(keys, vals, "side");
   double lots  = JD(keys, vals, "lots");
   double sl    = JD(keys, vals, "sl");
   double tp    = JD(keys, vals, "tp");
   long   slip  = JL(keys, vals, "max_slippage_pts");
   long   magic = JL(keys, vals, "magic");

   string reply = ExecuteMarketOrder(oid, side, lots, sl, tp, slip, magic);
   g_ids[g_ring_next]      = oid;
   g_replies[g_ring_next]  = reply;
   g_ring_next = (g_ring_next + 1) % ArraySize(g_ids);
   Send(reply);
}

string ExecuteMarketOrder(const string oid, const string side, const double lots,
                          const double sl_in, const double tp_in,
                          const long slip_pts_in, const long magic)
{
   MqlTick tk;
   if(!SymbolInfoTick(_Symbol, tk) || tk.bid <= 0.0 || tk.ask <= 0.0)
      return RejectJson(oid, 10018, "no quotes available");

   double vmin  = SymbolInfoDouble(_Symbol, SYMBOL_VOLUME_MIN);
   double vmax  = SymbolInfoDouble(_Symbol, SYMBOL_VOLUME_MAX);
   double vstep = SymbolInfoDouble(_Symbol, SYMBOL_VOLUME_STEP);
   if(lots < vmin - 1e-9)
      return RejectJson(oid, 10014, "lots below min " + DoubleToString(vmin, 2));
   if(lots > vmax + 1e-9)
      return RejectJson(oid, 10014, "lots above max " + DoubleToString(vmax, 2));
   if(vstep > 0.0)
   {
      double k = MathRound(lots / vstep);
      if(MathAbs(k * vstep - lots) > 1e-6)
         return RejectJson(oid, 10014, "lots not a multiple of step " + DoubleToString(vstep, 2));
   }
   if(sl_in <= 0.0)
      return RejectJson(oid, 10016, "missing stop loss (mandatory)");

   double sl = NormalizeDouble(sl_in, _Digits);
   double tp = (tp_in > 0.0) ? NormalizeDouble(tp_in, _Digits) : 0.0;
   bool is_buy  = (side == "buy");
   bool is_sell = (side == "sell");
   if(!is_buy && !is_sell)
      return RejectJson(oid, 10015, "unknown side: " + side);

   long   stops = SymbolInfoInteger(_Symbol, SYMBOL_TRADE_STOPS_LEVEL);
   double pt    = _Point;
   if(is_buy)
   {
      if(stops > 0 && sl > tk.bid - stops * pt)
         return RejectJson(oid, 10016, "sl too close for buy (stops_level=" +
                           IntegerToString(stops) + " bid=" + DoubleToString(tk.bid, _Digits) + ")");
      if(tp > 0.0 && stops > 0 && tp < tk.ask + stops * pt)
         return RejectJson(oid, 10016, "tp too close for buy");
   }
   else
   {
      if(stops > 0 && sl < tk.ask + stops * pt)
         return RejectJson(oid, 10016, "sl too close for sell (stops_level=" +
                           IntegerToString(stops) + " ask=" + DoubleToString(tk.ask, _Digits) + ")");
      if(tp > 0.0 && stops > 0 && tp > tk.bid - stops * pt)
         return RejectJson(oid, 10016, "tp too close for sell");
   }

   ResetLastError();
   g_trade.SetExpertMagicNumber((ulong)magic);
   g_trade.SetDeviationInPoints((ulong)MathAbs(slip_pts_in));
   g_trade.SetTypeFillingBySymbol(_Symbol);
   bool ok = is_buy ? g_trade.Buy(lots, _Symbol, 0.0, sl, tp, InpComment)
                    : g_trade.Sell(lots, _Symbol, 0.0, sl, tp, InpComment);
   uint rc = g_trade.ResultRetcode();
   if(ok && (rc == TRADE_RETCODE_DONE || rc == TRADE_RETCODE_DONE_PARTIAL ||
             rc == TRADE_RETCODE_PLACED))
   {
      double px     = g_trade.ResultPrice();
      long   ticket = (long)g_trade.ResultOrder();
      if(px <= 0.0 && PositionSelectByTicket((ulong)ticket))
         px = PositionGetDouble(POSITION_PRICE_OPEN);
      double realized_slip = 0.0;
      if(px > 0.0)
      {
         if(is_buy)
            realized_slip = MathRound((px - tk.ask) / pt);
         else
            realized_slip = MathRound((tk.bid - px) / pt);
      }
      Print("[bridge] filled order_id=", oid, " ticket=", ticket,
            " price=", DoubleToString(px, _Digits), " slip=", DoubleToString(realized_slip, 0));
      SendSnapshot();
      return FillJson(oid, ticket, px, lots, (long)realized_slip);
   }
   Print("[bridge] order failed oid=", oid, " rc=", rc, " err=", GetLastError(),
         " ", RetcodeText(rc));
   return RejectJson(oid, (long)rc, RetcodeText(rc));
}

void HandleModify(const string &keys[], const string &vals[])
{
   long   ticket = JL(keys, vals, "ticket");
   double sl     = JD(keys, vals, "sl");
   double tp     = JD(keys, vals, "tp");
   bool   ok     = g_trade.PositionModify((ulong)ticket, sl, tp);
   if(ok)
   {
      Print("[bridge] modify ok ticket=", ticket);
      SendSnapshot();
   }
   else
   {
      uint rc = g_trade.ResultRetcode();
      Print("[bridge] modify failed ticket=", ticket, " rc=", rc, " ", RetcodeText(rc));
      SendReject("modify-" + IntegerToString(ticket), (long)rc, "modify failed: " + RetcodeText(rc));
   }
}

void HandleClose(const string &keys[], const string &vals[])
{
   long   ticket = JL(keys, vals, "ticket");
   string oid    = JVal(keys, vals, "order_id"); // optional explicit ack id
   CloseTicket((ulong)ticket, oid);
}

void CloseTicket(const ulong ticket, const string oid_in)
{
   if(!PositionSelectByTicket(ticket))
   {
      Print("[bridge] close: no open position with ticket ", ticket);
      SendSnapshot(); // reconcile: engine sees no matching position
      return;
   }
   double lots = PositionGetDouble(POSITION_VOLUME);
   ResetLastError();
   bool ok = g_trade.PositionClose(ticket);
   if(ok)
   {
      double px   = g_trade.ResultPrice();
      long   deal = (long)g_trade.ResultDeal();
      if(px <= 0.0 && deal > 0 && HistoryDealSelect(deal))
         px = HistoryDealGetDouble(deal, DEAL_PRICE);
      string oid = (oid_in == "") ? ("close-" + IntegerToString((long)ticket)) : oid_in;
      Print("[bridge] closed ticket=", ticket, " price=", DoubleToString(px, _Digits));
      Send(FillJson(oid, (long)ticket, px, lots, 0));
      SendSnapshot();
   }
   else
   {
      uint rc = g_trade.ResultRetcode();
      Print("[bridge] close failed ticket=", ticket, " rc=", rc, " ", RetcodeText(rc));
      string oid = (oid_in == "") ? ("close-" + IntegerToString((long)ticket)) : oid_in;
      SendReject(oid, (long)rc, "close failed: " + RetcodeText(rc));
   }
}

void HandleCloseAll(const string &keys[], const string &vals[])
{
   string reason = JVal(keys, vals, "reason");
   Print("[bridge] close_all reason=", reason);
   int closed = 0;
   for(int i = PositionsTotal() - 1; i >= 0; i--)
   {
      ulong ticket = PositionGetTicket(i);
      if(ticket == 0)
         continue;
      if(PositionGetString(POSITION_SYMBOL) != _Symbol)
         continue;
      long pm = PositionGetInteger(POSITION_MAGIC);
      if(InpMagic != 0 && pm != InpMagic)
         continue;
      CloseTicket(ticket, "");
      closed++;
   }
   Print("[bridge] close_all closed=", closed);
   SendSnapshot();
}

//+------------------------------------------------------------------+
//| Outbound: snapshots                                              |
//+------------------------------------------------------------------+
void SendHelloAndSnapshot()
{
   string msg = "{\"type\":\"hello\",\"seq\":" + NextSeq() +
                ",\"account\":" + IntegerToString(AccountInfoInteger(ACCOUNT_LOGIN)) +
                ",\"server\":\"" + JEsc(AccountInfoString(ACCOUNT_SERVER)) + "\"" +
                ",\"symbol\":\"" + JEsc(_Symbol) + "\"" +
                ",\"digits\":" + IntegerToString(_Digits) +
                ",\"point\":" + Dbl(_Point, 8) +
                ",\"contract_size\":" + Dbl(SymbolInfoDouble(_Symbol, SYMBOL_TRADE_CONTRACT_SIZE), 2) +
                ",\"min_lot\":" + Dbl(SymbolInfoDouble(_Symbol, SYMBOL_VOLUME_MIN), 2) +
                ",\"lot_step\":" + Dbl(SymbolInfoDouble(_Symbol, SYMBOL_VOLUME_STEP), 2) +
                ",\"server_utc_offset_s\":" + IntegerToString((long)TimeCurrent() - (long)TimeGMT()) +
                ",\"stops_level_pts\":" + IntegerToString(SymbolInfoInteger(_Symbol, SYMBOL_TRADE_STOPS_LEVEL)) +
                ",\"demo\":" + (AccountInfoInteger(ACCOUNT_TRADE_MODE) == ACCOUNT_TRADE_MODE_DEMO ? "true" : "false") +
                "}";
   Send(msg);
   SendSnapshot();
}

void SendSnapshot()
{
   string acc = "{\"type\":\"account\",\"seq\":" + NextSeq() +
                ",\"balance\":" + Dbl(AccountInfoDouble(ACCOUNT_BALANCE), 2) +
                ",\"equity\":" + Dbl(AccountInfoDouble(ACCOUNT_EQUITY), 2) +
                ",\"margin\":" + Dbl(AccountInfoDouble(ACCOUNT_MARGIN), 2) +
                ",\"free_margin\":" + Dbl(AccountInfoDouble(ACCOUNT_MARGIN_FREE), 2) + "}";
   Send(acc);
   for(int i = PositionsTotal() - 1; i >= 0; i--)
   {
      ulong ticket = PositionGetTicket(i);
      if(ticket == 0)
         continue;
      if(PositionGetString(POSITION_SYMBOL) != _Symbol)
         continue;
      long pm = PositionGetInteger(POSITION_MAGIC);
      if(InpMagic != 0 && pm != InpMagic)
         continue;
      long ptype = PositionGetInteger(POSITION_TYPE);
      string pos = "{\"type\":\"position\",\"seq\":" + NextSeq() +
                   ",\"ticket\":" + IntegerToString((long)ticket) +
                   ",\"side\":\"" + (ptype == POSITION_TYPE_BUY ? "buy" : "sell") + "\"" +
                   ",\"lots\":" + Dbl(PositionGetDouble(POSITION_VOLUME), 2) +
                   ",\"open_price\":" + Dbl(PositionGetDouble(POSITION_PRICE_OPEN), _Digits) +
                   ",\"sl\":" + Dbl(PositionGetDouble(POSITION_SL), _Digits) +
                   ",\"tp\":" + Dbl(PositionGetDouble(POSITION_TP), _Digits) + "}";
      Send(pos);
   }
}

//+------------------------------------------------------------------+
//| Outbound primitives                                              |
//+------------------------------------------------------------------+
bool Send(const string line)
{
   if(g_sock == INVALID_HANDLE)
      return false;
   uchar data[];
   int n = StringToCharArray(line + "\n", data, 0, WHOLE_ARRAY, CP_UTF8) - 1; // drop NUL
   if(n <= 0)
      return false;
   ArrayResize(data, n);
   ResetLastError();
   int sent = SocketWrite(g_sock, data);
   if(sent != n)
   {
      Print("[bridge] SocketWrite failed sent=", sent, " of ", n, " err=", GetLastError());
      Disconnect("write failed");
      return false;
   }
   return true;
}

string NextSeq()
{
   g_out_seq++;
   return IntegerToString(g_out_seq);
}

string Dbl(const double v, const int digits)
{
   return DoubleToString(v, digits);
}

string JEsc(const string s)
{
   string out = "";
   int n = StringLen(s);
   for(int i = 0; i < n; i++)
   {
      ushort c = StringGetCharacter(s, i);
      if(c == '\\' || c == '"')
         out += "\\";
      out += ShortToString(c);
   }
   return out;
}

string FillJson(const string order_id, const long ticket, const double price,
                const double lots, const long slippage_pts)
{
   return "{\"type\":\"fill\",\"seq\":" + NextSeq() +
          ",\"order_id\":\"" + JEsc(order_id) + "\"" +
          ",\"ticket\":" + IntegerToString(ticket) +
          ",\"price\":" + Dbl(price, _Digits) +
          ",\"lots\":" + Dbl(lots, 2) +
          ",\"time\":" + IntegerToString((long)TimeCurrent()) +
          ",\"slippage_pts\":" + IntegerToString(slippage_pts) + "}";
}

string RejectJson(const string order_id, const long retcode, const string reason)
{
   return "{\"type\":\"order_reject\",\"seq\":" + NextSeq() +
          ",\"order_id\":\"" + JEsc(order_id) + "\"" +
          ",\"retcode\":" + IntegerToString(retcode) +
          ",\"reason\":\"" + JEsc(reason) + "\"}";
}

string RetcodeText(const uint rc)
{
   switch(rc)
   {
      case TRADE_RETCODE_REQUOTE:           return "requote";
      case TRADE_RETCODE_REJECT:            return "order rejected";
      case TRADE_RETCODE_CANCEL:            return "canceled";
      case TRADE_RETCODE_PLACED:            return "placed";
      case TRADE_RETCODE_DONE:              return "done";
      case TRADE_RETCODE_DONE_PARTIAL:      return "done partial";
      case TRADE_RETCODE_ERROR:             return "request error";
      case TRADE_RETCODE_TIMEOUT:           return "timeout";
      case TRADE_RETCODE_INVALID:           return "invalid request";
      case TRADE_RETCODE_INVALID_VOLUME:    return "invalid volume";
      case TRADE_RETCODE_INVALID_PRICE:     return "invalid price";
      case TRADE_RETCODE_INVALID_STOPS:     return "invalid stops";
      case TRADE_RETCODE_TRADE_DISABLED:    return "trading disabled";
      case TRADE_RETCODE_MARKET_CLOSED:     return "market closed";
      case TRADE_RETCODE_NO_MONEY:          return "no money";
      case TRADE_RETCODE_PRICE_CHANGED:     return "price changed";
      case TRADE_RETCODE_PRICE_OFF:         return "no quotes";
      case TRADE_RETCODE_INVALID_EXPIRATION:return "invalid expiration";
      case TRADE_RETCODE_ORDER_CHANGED:     return "order changed";
      case TRADE_RETCODE_TOO_MANY_REQUESTS: return "too many requests";
      case TRADE_RETCODE_NO_CHANGES:        return "no changes";
      case TRADE_RETCODE_CLIENT_DISABLES_AT:return "autotrading disabled by client terminal";
      case TRADE_RETCODE_SERVER_DISABLES_AT:return "autotrading disabled by server";
      case TRADE_RETCODE_LOCKED:            return "request locked for processing";
      case TRADE_RETCODE_FROZEN:            return "order or position frozen";
      case TRADE_RETCODE_INVALID_FILL:      return "invalid fill type";
      case TRADE_RETCODE_CONNECTION:        return "no connection with trade server";
      case TRADE_RETCODE_ONLY_REAL:         return "only real accounts allowed";
      case TRADE_RETCODE_LIMIT_ORDERS:      return "orders limit reached";
      case TRADE_RETCODE_LIMIT_VOLUME:      return "volume limit reached";
      default:                              return "retcode " + IntegerToString((long)rc);
   }
}

//+------------------------------------------------------------------+
//| Minimal flat-JSON parser (objects with string/number/bool/null   |
//| values only — exactly what the engine protocol sends).           |
//+------------------------------------------------------------------+
bool IsWs(const ushort c)
{
   return c == ' ' || c == '\t' || c == '\r' || c == '\n';
}

long HexVal(const string hex)
{
   long v = 0;
   int n = StringLen(hex);
   for(int i = 0; i < n; i++)
   {
      ushort c = StringGetCharacter(hex, i);
      long d;
      if(c >= '0' && c <= '9') d = (long)(c - '0');
      else if(c >= 'a' && c <= 'f') d = (long)(c - 'a') + 10;
      else if(c >= 'A' && c <= 'F') d = (long)(c - 'A') + 10;
      else return -1;
      v = v * 16 + d;
   }
   return v;
}

// Reads a JSON string starting at json[start] == '"'; returns the index
// just after the closing quote, or -1 on error.
int ReadJsonString(const string json, const int start, string &out)
{
   int n = StringLen(json);
   int i = start + 1;
   out = "";
   while(i < n)
   {
      ushort c = StringGetCharacter(json, i);
      if(c == '\\')
      {
         if(i + 1 >= n)
            return -1;
         ushort e = StringGetCharacter(json, i + 1);
         if(e == 'u')
         {
            if(i + 5 >= n)
               return -2;
            long code = HexVal(StringSubstr(json, i + 2, 4));
            if(code < 0)
               return -3;
            out += ShortToString((ushort)code);
            i += 6;
            continue;
         }
         if(e == '"')       out += "\"";
         else if(e == '\\') out += "\\";
         else if(e == '/')  out += "/";
         else if(e == 'n')  out += "\n";
         else if(e == 't')  out += "\t";
         else if(e == 'r')  out += "\r";
         else               out += ShortToString(e);
         i += 2;
         continue;
      }
      if(c == '"')
         return i + 1;
      out += ShortToString(c);
      i++;
   }
   return -4;
}

// Reads a bare JSON token (number/true/false/null); returns the index of
// the terminator (',' or '}') or -1 on error.
int ReadJsonRaw(const string json, const int start, string &out)
{
   int n = StringLen(json);
   int i = start;
   out = "";
   while(i < n)
   {
      ushort c = StringGetCharacter(json, i);
      if(IsWs(c))
      {
         i++;
         continue;
      }
      if(c == ',' || c == '}')
         return i;
      out += ShortToString(c);
      i++;
   }
   return -1;
}

// Parses a flat JSON object into parallel key/value arrays.
// Values: strings stored unescaped, others stored as raw text.
int JsonParseFlat(const string json, string &keys[], string &vals[])
{
   ArrayResize(keys, 0);
   ArrayResize(vals, 0);
   int n = StringLen(json);
   int i = 0;
   while(i < n && StringGetCharacter(json, i) != '{')
      i++;
   if(i >= n)
      return -1;
   i++;
   while(i < n)
   {
      while(i < n && IsWs(StringGetCharacter(json, i)))
         i++;
      if(i >= n)
         return -2;
      ushort c = StringGetCharacter(json, i);
      if(c == '}')
         return 0;
      if(c != '"')
         return -3;
      string key = "";
      i = ReadJsonString(json, i, key);
      if(i < 0)
         return -4;
      while(i < n && IsWs(StringGetCharacter(json, i)))
         i++;
      if(i >= n || StringGetCharacter(json, i) != ':')
         return -5;
      i++;
      while(i < n && IsWs(StringGetCharacter(json, i)))
         i++;
      if(i >= n)
         return -6;
      string val = "";
      if(StringGetCharacter(json, i) == '"')
      {
         i = ReadJsonString(json, i, val);
         if(i < 0)
            return -7;
      }
      else
      {
         i = ReadJsonRaw(json, i, val);
         if(i < 0)
            return -8;
      }
      int nk = ArraySize(keys);
      ArrayResize(keys, nk + 1);
      ArrayResize(vals, nk + 1);
      keys[nk] = key;
      vals[nk] = val;
      while(i < n && IsWs(StringGetCharacter(json, i)))
         i++;
      if(i >= n)
         return -9;
      c = StringGetCharacter(json, i);
      if(c == '}')
         return 0;
      if(c != ',')
         return -10;
      i++;
   }
   return -11;
}

string JVal(const string &keys[], const string &vals[], const string key)
{
   for(int i = 0; i < ArraySize(keys); i++)
      if(keys[i] == key)
         return vals[i];
   return "";
}

double JD(const string &keys[], const string &vals[], const string key)
{
   string v = JVal(keys, vals, key);
   if(v == "" || v == "true" || v == "false" || v == "null")
      return 0.0;
   return StringToDouble(v);
}

long JL(const string &keys[], const string &vals[], const string key)
{
   string v = JVal(keys, vals, key);
   if(v == "" || v == "true" || v == "false" || v == "null")
      return 0;
   return StringToInteger(v);
}
//+------------------------------------------------------------------+

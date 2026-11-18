//+------------------------------------------------------------------+
//| ExportHistory.mq5                                                |
//| Phase 1: export symbol bar history + a recent tick sample to CSV |
//| under MQL5\Files, for the Rust data pipeline.                    |
//| Run on ANY chart - the export target symbol is an input, so the  |
//| Bridge EA on the XAUUSD chart is never touched.                  |
//|                                                                  |
//| Bars CSV columns:                                                |
//|   time,open,high,low,close,tick_volume,spread,real_volume,       |
//|   time_epoch                                                     |
//| `time` = broker/server-local "yyyy.mm.dd hh:mi" (TimeToString),  |
//| `time_epoch` = Unix seconds in the SAME server-local clock. The  |
//| Rust loader converts to UTC with the configured offset.          |
//|                                                                  |
//| Ticks CSV columns:                                               |
//|   time_msc,bid,ask,last,volume_real,flags,time                   |
//| `time_msc` = Unix milliseconds, server-local clock (same rule).  |
//|                                                                  |
//| History is copied year-by-year (ticks day-by-day) with a         |
//| stability wait per chunk, so download-on-demand works and memory |
//| stays bounded. Journal markers: EXPORT_BARS / EXPORT_TICKS /     |
//| EXPORT_DONE / EXPORT_FAIL.                                       |
//+------------------------------------------------------------------+
#property copyright "filthi-bot"
#property version   "1.00"
#property description "Export bar history + tick sample to CSV for the Rust pipeline."
#property strict

input string InpSymbol       = "XAUUSD";
input string InpTimeframe    = "M1";
input string InpStartDate    = "2020.01.01";  // empty = probe from 2018
input string InpEndDate      = "";            // empty = now
input string InpBarsFile     = "xauusd_m1.csv";
input string InpTicksFile    = "xauusd_ticks.csv";
input int    InpTicksDays    = 5;             // tick sample length, days
input int    InpMaxWaitS     = 90;            // per-year stability wait (bars)
input int    InpTickMaxWaitS = 180;           // per-day stability wait (ticks)

//+------------------------------------------------------------------+
//| Timeframe name -> enum (manual map: no surprises under MCP)      |
//+------------------------------------------------------------------+
ENUM_TIMEFRAMES ParseTF(const string s)
{
   if(s == "M1")  return PERIOD_M1;
   if(s == "M2")  return PERIOD_M2;
   if(s == "M3")  return PERIOD_M3;
   if(s == "M4")  return PERIOD_M4;
   if(s == "M5")  return PERIOD_M5;
   if(s == "M6")  return PERIOD_M6;
   if(s == "M10") return PERIOD_M10;
   if(s == "M12") return PERIOD_M12;
   if(s == "M15") return PERIOD_M15;
   if(s == "M20") return PERIOD_M20;
   if(s == "M30") return PERIOD_M30;
   if(s == "H1")  return PERIOD_H1;
   if(s == "H2")  return PERIOD_H2;
   if(s == "H3")  return PERIOD_H3;
   if(s == "H4")  return PERIOD_H4;
   if(s == "H6")  return PERIOD_H6;
   if(s == "H8")  return PERIOD_H8;
   if(s == "H12") return PERIOD_H12;
   if(s == "D1")  return PERIOD_D1;
   if(s == "W1")  return PERIOD_W1;
   if(s == "MN1") return PERIOD_MN1;
   return PERIOD_CURRENT;
}

//+------------------------------------------------------------------+
//| Copy rates for [t0,t1] until the count is stable for 2 consecutive|
//| seconds-apart calls or the wait budget expires. Returns the count|
//| of the LAST successful copy (rates[] matches it); 0 if none.     |
//+------------------------------------------------------------------+
int CopyRatesStable(const string sym, const ENUM_TIMEFRAMES tf,
                    const datetime t0, const datetime t1,
                    MqlRates &rates[], const int max_wait_s)
{
   int cur = 0;
   int stable = 0;
   ulong tms = GetTickCount64();
   while(true)
   {
      ResetLastError();
      int n = CopyRates(sym, tf, t0, t1, rates);
      if(n > 0)
      {
         if(n == cur)
         {
            stable++;
            if(stable >= 2)
               return n;
         }
         else
            stable = 1;
         cur = n;
      }
      if((GetTickCount64() - tms) >= (ulong)max_wait_s * 1000)
         return cur;
      Sleep(1000);
   }
}

//+------------------------------------------------------------------+
//| Same idea for ticks (per-day chunks). Returns last good count.   |
//+------------------------------------------------------------------+
int CopyTicksStable(const string sym, MqlTick &ticks[],
                    const long from_ms, const long to_ms, const int max_wait_s)
{
   int cur = 0;
   int stable = 0;
   ulong tms = GetTickCount64();
   while(true)
   {
      ResetLastError();
      int n = CopyTicksRange(sym, ticks, COPY_TICKS_ALL, from_ms, to_ms);
      if(n > 0)
      {
         if(n == cur)
         {
            stable++;
            if(stable >= 2)
               return n;
         }
         else
            stable = 1;
         cur = n;
      }
      if((GetTickCount64() - tms) >= (ulong)max_wait_s * 1000)
         return cur;
      Sleep(1000);
   }
}

//+------------------------------------------------------------------+
//| Script entry                                                     |
//+------------------------------------------------------------------+
void OnStart()
{
   const ENUM_TIMEFRAMES tf = ParseTF(InpTimeframe);
   if(tf == PERIOD_CURRENT)
   {
      Print("[export] EXPORT_FAIL unknown timeframe '", InpTimeframe, "'");
      return;
   }

   if(!SymbolSelect(InpSymbol, true))
   {
      Print("[export] EXPORT_FAIL symbol not available: ", InpSymbol, " err=", GetLastError());
      return;
   }

   const int digits = (int)SymbolInfoInteger(InpSymbol, SYMBOL_DIGITS);

   // ---- time range -------------------------------------------------
   const datetime t_end = (InpEndDate == "") ? TimeCurrent() : StringToTime(InpEndDate + " 23:59");
   datetime t_start = 0;
   int year0;
   if(InpStartDate == "")
   {
      year0 = (int)StringToInteger(StringSubstr(TimeToString(TimeCurrent(), TIME_DATE), 0, 4)) - 8; // probe ~8 years back
      t_start = StringToTime(IntegerToString(year0) + ".01.01 00:00");
   }
   else
   {
      t_start = StringToTime(InpStartDate + " 00:00");
      year0 = (int)StringToInteger(StringSubstr(InpStartDate, 0, 4)); // exact year from "yyyy.mm.dd"
   }
   const int year_now = (int)StringToInteger(StringSubstr(TimeToString(TimeCurrent(), TIME_DATE), 0, 4));

   Print("[export] start symbol=", InpSymbol, " tf=", InpTimeframe,
         " range=", (t_start > 0 ? TimeToString(t_start, TIME_DATE) : "earliest"),
         " .. ", TimeToString(t_end, TIME_DATE), " digits=", digits);

   // ---- bars -------------------------------------------------------
   int fh = FileOpen(InpBarsFile, FILE_WRITE | FILE_TXT | FILE_ANSI);
   if(fh == INVALID_HANDLE)
   {
      Print("[export] EXPORT_FAIL cannot open ", InpBarsFile, " err=", GetLastError());
      return;
   }
   FileWrite(fh, "time,open,high,low,close,tick_volume,spread,real_volume,time_epoch");

   long bars_rows = 0;
   datetime first_bar = 0, last_bar = 0;
   for(int y = year0; y <= year_now; y++)
   {
      datetime ys = (y == year0 && t_start > 0) ? t_start : StringToTime(IntegerToString(y) + ".01.01 00:00");
      datetime ye = StringToTime(IntegerToString(y) + ".12.31 23:59");
      if(ye > t_end)
         ye = t_end;
      if(ys > ye)
         continue;

      MqlRates rates[];
      ArraySetAsSeries(rates, false);            // index 0 = oldest
      const int n = CopyRatesStable(InpSymbol, tf, ys, ye, rates, InpMaxWaitS);
      if(n <= 0)
      {
         Print("[export] year ", y, ": no data (count=0 err=", GetLastError(), ")");
         continue;
      }
      for(int i = 0; i < n; i++)
      {
         FileWrite(fh, StringFormat("%s,%s,%s,%s,%s,%I64d,%d,%I64d,%I64d",
                                    TimeToString(rates[i].time, TIME_DATE | TIME_MINUTES),
                                    DoubleToString(rates[i].open, digits),
                                    DoubleToString(rates[i].high, digits),
                                    DoubleToString(rates[i].low, digits),
                                    DoubleToString(rates[i].close, digits),
                                    rates[i].tick_volume,
                                    rates[i].spread,
                                    rates[i].real_volume,
                                    (long)rates[i].time));
         if(first_bar == 0)
            first_bar = rates[0].time;
         last_bar = rates[n - 1].time;
      }
      bars_rows += n;
      Print("[export] year ", y, ": ", n, " bars (total ", bars_rows, ")");
   }
   FileClose(fh);
   Print("[export] EXPORT_BARS rows=", bars_rows,
         " first=", TimeToString(first_bar, TIME_DATE | TIME_MINUTES),
         " last=", TimeToString(last_bar, TIME_DATE | TIME_MINUTES),
         " file=", InpBarsFile);

   // ---- ticks (last InpTicksDays days) ------------------------------
   const long end_ms = (long)TimeCurrent() * 1000;
   const long begin_ms = end_ms - (long)InpTicksDays * 86400 * 1000;

   fh = FileOpen(InpTicksFile, FILE_WRITE | FILE_TXT | FILE_ANSI);
   if(fh == INVALID_HANDLE)
   {
      Print("[export] EXPORT_FAIL cannot open ", InpTicksFile, " err=", GetLastError());
      return;
   }
   FileWrite(fh, "time_msc,bid,ask,last,volume_real,flags,time");

   long tick_rows = 0;
   datetime d = (datetime)(begin_ms / 1000);
   while(d <= (datetime)(end_ms / 1000))
   {
      datetime d1 = d + 86399;
      if(d1 > (datetime)(end_ms / 1000))
         d1 = (datetime)(end_ms / 1000);

      MqlTick ticks[];
      const int n = CopyTicksStable(InpSymbol, ticks, (long)d * 1000, (long)d1 * 1000 + 999, InpTickMaxWaitS);
      if(n > 0)
      {
         for(int i = 0; i < n; i++)
         {
            FileWrite(fh, StringFormat("%I64d,%s,%s,%s,%.2f,%d,%s",
                                       ticks[i].time_msc,
                                       DoubleToString(ticks[i].bid, digits),
                                       DoubleToString(ticks[i].ask, digits),
                                       DoubleToString(ticks[i].last, digits),
                                       (ticks[i].volume_real > 0 ? ticks[i].volume_real : (double)ticks[i].volume),
                                       ticks[i].flags,
                                       TimeToString(ticks[i].time, TIME_DATE | TIME_SECONDS)));
         }
         tick_rows += n;
         Print("[export] tick day ", TimeToString(d, TIME_DATE), ": ", n, " ticks (total ", tick_rows, ")");
      }
      d += 86400;
   }
   FileClose(fh);
   Print("[export] EXPORT_TICKS rows=", tick_rows, " file=", InpTicksFile);
   Print("[export] EXPORT_DONE");
}
//+------------------------------------------------------------------+

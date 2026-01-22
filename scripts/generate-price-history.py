#!/usr/bin/env python3
"""Generate mock historical price data for ITPs."""

import random
from datetime import datetime, timedelta

# TOP5 ITP parameters
itp_id = "0x0b0aAF97852741A5F9Ac8db25D53E3401861629e"
base_price = 1000.0  # Initial price in USD

# Generate 7 days of hourly data (168 hours)
now = datetime.utcnow()
start_time = now - timedelta(days=7)

# Also generate 5-min data for last 24 hours
start_5min = now - timedelta(hours=24)

sql_statements = []

# Simulate price movement with random walk
price = base_price
random.seed(42)  # Reproducible

# Generate hourly data (7 days)
for hour in range(168):
    timestamp = start_time + timedelta(hours=hour)
    # Random walk: +/- 1% per hour
    change = random.uniform(-0.01, 0.01)
    price = price * (1 + change)
    price = max(price, base_price * 0.8)  # Floor at 80% of initial
    price = min(price, base_price * 1.2)  # Cap at 120% of initial

    sql = f"""INSERT INTO itp_price_history (itp_id, price, volume, timestamp, granularity)
VALUES ('{itp_id}', {price:.6f}, NULL, '{timestamp.isoformat()}+00', 'hourly')
ON CONFLICT DO NOTHING;"""
    sql_statements.append(sql)

# Generate 5-min data (last 24 hours = 288 intervals)
price_5min = price  # Continue from hourly price
for interval in range(288):
    timestamp = start_5min + timedelta(minutes=5*interval)
    change = random.uniform(-0.003, 0.003)  # Smaller variance for 5-min
    price_5min = price_5min * (1 + change)
    price_5min = max(price_5min, base_price * 0.8)
    price_5min = min(price_5min, base_price * 1.2)

    sql = f"""INSERT INTO itp_price_history (itp_id, price, volume, timestamp, granularity)
VALUES ('{itp_id}', {price_5min:.6f}, NULL, '{timestamp.isoformat()}+00', '5min')
ON CONFLICT DO NOTHING;"""
    sql_statements.append(sql)

# Generate daily data (30 days for "all" view)
price_daily = base_price
start_daily = now - timedelta(days=30)
for day in range(30):
    timestamp = start_daily + timedelta(days=day)
    change = random.uniform(-0.02, 0.02)  # +/- 2% per day
    price_daily = price_daily * (1 + change)
    price_daily = max(price_daily, base_price * 0.7)
    price_daily = min(price_daily, base_price * 1.3)

    sql = f"""INSERT INTO itp_price_history (itp_id, price, volume, timestamp, granularity)
VALUES ('{itp_id}', {price_daily:.6f}, NULL, '{timestamp.isoformat()}+00', 'daily')
ON CONFLICT DO NOTHING;"""
    sql_statements.append(sql)

print('\n'.join(sql_statements))

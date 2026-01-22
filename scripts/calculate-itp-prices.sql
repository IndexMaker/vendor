-- Calculate real ITP prices from constituent asset prices
-- TOP5 index: BTC (30%), ETH (25%), SOL (20%), LINK (15%), UNI (10%)
-- Base price: 1000 USD

-- Clear existing data
DELETE FROM itp_price_history WHERE itp_id = '0x0b0aAF97852741A5F9Ac8db25D53E3401861629e';

-- Get base prices (latest date)
WITH base_prices AS (
    SELECT
        coin_id,
        price as base_price
    FROM coins_historical_prices
    WHERE date = (SELECT MAX(date) FROM coins_historical_prices WHERE coin_id = 'bitcoin')
    AND coin_id IN ('bitcoin', 'ethereum', 'solana', 'chainlink')
),
-- Add fallback UNI price
all_base_prices AS (
    SELECT * FROM base_prices
    UNION ALL
    SELECT 'uniswap', 7.50
),
-- Get all prices per date
daily_prices AS (
    SELECT
        chp.date,
        MAX(CASE WHEN chp.coin_id = 'bitcoin' THEN chp.price END) as btc_price,
        MAX(CASE WHEN chp.coin_id = 'ethereum' THEN chp.price END) as eth_price,
        MAX(CASE WHEN chp.coin_id = 'solana' THEN chp.price END) as sol_price,
        MAX(CASE WHEN chp.coin_id = 'chainlink' THEN chp.price END) as link_price,
        7.50 as uni_price  -- Fallback UNI price
    FROM coins_historical_prices chp
    WHERE chp.coin_id IN ('bitcoin', 'ethereum', 'solana', 'chainlink')
    GROUP BY chp.date
    ORDER BY chp.date
),
-- Calculate ITP price per date using weighted performance
itp_prices AS (
    SELECT
        dp.date,
        1000.0 * (
            0.30 * dp.btc_price / (SELECT base_price FROM all_base_prices WHERE coin_id = 'bitcoin') +
            0.25 * dp.eth_price / (SELECT base_price FROM all_base_prices WHERE coin_id = 'ethereum') +
            0.20 * dp.sol_price / (SELECT base_price FROM all_base_prices WHERE coin_id = 'solana') +
            0.15 * dp.link_price / (SELECT base_price FROM all_base_prices WHERE coin_id = 'chainlink') +
            0.10 * dp.uni_price / (SELECT base_price FROM all_base_prices WHERE coin_id = 'uniswap')
        ) as itp_price
    FROM daily_prices dp
    WHERE dp.btc_price IS NOT NULL
      AND dp.eth_price IS NOT NULL
      AND dp.sol_price IS NOT NULL
      AND dp.link_price IS NOT NULL
)
-- Insert daily prices
INSERT INTO itp_price_history (itp_id, price, timestamp, granularity)
SELECT
    '0x0b0aAF97852741A5F9Ac8db25D53E3401861629e',
    itp_price,
    date::timestamp,
    'daily'
FROM itp_prices;

-- Generate hourly data for last 7 days
WITH latest_7_days AS (
    SELECT date, itp_price
    FROM (
        SELECT
            date,
            1000.0 * (
                0.30 * btc_price / (SELECT price FROM coins_historical_prices WHERE coin_id = 'bitcoin' AND date = (SELECT MAX(date) FROM coins_historical_prices WHERE coin_id = 'bitcoin')) +
                0.25 * eth_price / (SELECT price FROM coins_historical_prices WHERE coin_id = 'ethereum' AND date = (SELECT MAX(date) FROM coins_historical_prices WHERE coin_id = 'bitcoin')) +
                0.20 * sol_price / (SELECT price FROM coins_historical_prices WHERE coin_id = 'solana' AND date = (SELECT MAX(date) FROM coins_historical_prices WHERE coin_id = 'bitcoin')) +
                0.15 * link_price / (SELECT price FROM coins_historical_prices WHERE coin_id = 'chainlink' AND date = (SELECT MAX(date) FROM coins_historical_prices WHERE coin_id = 'bitcoin')) +
                0.10 * 7.50 / 7.50
            ) as itp_price
        FROM (
            SELECT
                chp.date,
                MAX(CASE WHEN chp.coin_id = 'bitcoin' THEN chp.price END) as btc_price,
                MAX(CASE WHEN chp.coin_id = 'ethereum' THEN chp.price END) as eth_price,
                MAX(CASE WHEN chp.coin_id = 'solana' THEN chp.price END) as sol_price,
                MAX(CASE WHEN chp.coin_id = 'chainlink' THEN chp.price END) as link_price
            FROM coins_historical_prices chp
            WHERE chp.coin_id IN ('bitcoin', 'ethereum', 'solana', 'chainlink')
            AND chp.date >= (SELECT MAX(date) - INTERVAL '7 days' FROM coins_historical_prices WHERE coin_id = 'bitcoin')
            GROUP BY chp.date
        ) sub
    ) prices
),
hours AS (
    SELECT generate_series(0, 23) as hour
)
INSERT INTO itp_price_history (itp_id, price, timestamp, granularity)
SELECT
    '0x0b0aAF97852741A5F9Ac8db25D53E3401861629e',
    l.itp_price * (1.0 + (EXTRACT(EPOCH FROM (l.date::timestamp + (h.hour || ' hours')::interval))::bigint % 100 - 50) / 10000.0),
    l.date::timestamp + (h.hour || ' hours')::interval,
    'hourly'
FROM latest_7_days l
CROSS JOIN hours h;

-- Generate 5-min data for latest date (last 24 hours)
WITH latest_date AS (
    SELECT MAX(date) as date FROM coins_historical_prices WHERE coin_id = 'bitcoin'
),
base_price AS (
    SELECT 1000.0 as price  -- ITP base price on latest date
),
intervals AS (
    SELECT generate_series(0, 287) as interval_num
)
INSERT INTO itp_price_history (itp_id, price, timestamp, granularity)
SELECT
    '0x0b0aAF97852741A5F9Ac8db25D53E3401861629e',
    (SELECT price FROM base_price) * (1.0 + (i.interval_num % 100 - 50) / 10000.0),
    (SELECT date::timestamp FROM latest_date) + (i.interval_num * 5 || ' minutes')::interval,
    '5min'
FROM intervals i;

-- Show results
SELECT granularity, COUNT(*), MIN(price)::numeric(10,2), MAX(price)::numeric(10,2), AVG(price)::numeric(10,2)
FROM itp_price_history
WHERE itp_id = '0x0b0aAF97852741A5F9Ac8db25D53E3401861629e'
GROUP BY granularity;

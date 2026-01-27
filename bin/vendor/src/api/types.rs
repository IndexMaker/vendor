use serde::{Deserialize, Serialize};

/// Request to get quotes for specific assets
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuoteAssetsRequest {
    /// Asset IDs (Labels) to quote
    pub assets: Vec<u128>,
}

/// Response with market data for stale assets only
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetsQuote {
    /// Asset IDs (stale subset only)
    pub assets: Vec<u128>,
    
    /// Liquidity for each asset (in base asset units)
    pub liquidity: Vec<f64>,
    
    /// Prices for each asset (in USD)
    pub prices: Vec<f64>,
    
    /// Slopes for each asset (price impact measure)
    pub slopes: Vec<f64>,
}

impl AssetsQuote {
    pub fn new() -> Self {
        Self {
            assets: Vec::new(),
            liquidity: Vec::new(),
            prices: Vec::new(),
            slopes: Vec::new(),
        }
    }
    
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
    
    pub fn len(&self) -> usize {
        self.assets.len()
    }
}

/// Health check response
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub vendor_id: u128,
    pub tracked_assets: usize,
}

/// Error response
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Request from Keeper to process assets (Story 3-4)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessAssetsRequest {
    /// Asset IDs to process
    pub asset_ids: Vec<u128>,
    /// Batch correlation ID for tracing
    pub batch_id: String,
}

/// Response for process-assets endpoint
#[derive(Debug, Clone, Serialize)]
pub struct ProcessAssetsResponse {
    /// Batch correlation ID
    pub batch_id: String,
    /// Number of assets processed
    pub assets_processed: usize,
    /// Timing metrics
    pub metrics: ProcessMetrics,
}

/// Timing metrics for response
#[derive(Debug, Clone, Serialize)]
pub struct ProcessMetrics {
    pub margin_ms: u64,
    pub supply_ms: u64,
    pub market_data_ms: u64,
    pub total_ms: u64,
}

// ============================================================================
// Story 3-6: Async Buffer Types
// ============================================================================

/// Request to queue orders for async processing (Story 3-6, AC #1)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueueOrdersRequest {
    /// Orders to queue
    pub orders: Vec<OrderRequest>,
    /// Batch correlation ID for tracing
    pub batch_id: String,
}

/// Individual order in a queue request
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderRequest {
    /// Asset ID (on-chain label)
    pub asset_id: u128,
    /// Symbol for exchange (e.g., "BTCUSDT")
    pub symbol: String,
    /// "buy" or "sell"
    pub side: String,
    /// Quantity to trade
    pub quantity: f64,
}

/// Response for queue-orders endpoint (immediate return < 100ms)
#[derive(Debug, Clone, Serialize)]
pub struct QueueOrdersResponse {
    /// Whether orders were accepted
    pub accepted: bool,
    /// Number of orders queued
    pub queued_count: usize,
    /// Batch correlation ID
    pub batch_id: String,
    /// Current queue depth
    pub queue_depth: usize,
    /// Estimated processing time hint (optional)
    pub estimated_processing_ms: Option<u64>,
}

/// Buffer status for health endpoint
#[derive(Debug, Clone, Serialize)]
pub struct BufferStatus {
    /// Number of orders in queue
    pub queue_depth: usize,
    /// Number of assets being buffered (below min size)
    pub buffered_assets: usize,
    /// Total orders processed since startup
    pub total_processed: u64,
    /// Oldest order age in milliseconds
    pub oldest_order_age_ms: i64,
}

/// Extended health response with buffer info (Story 3-6, AC #6.3)
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponseWithBuffer {
    pub status: String,
    pub vendor_id: u128,
    pub tracked_assets: usize,
    /// Buffer status (if async processing is enabled)
    pub buffer: Option<BufferStatus>,
}

// ============================================================================
// Keeper Integration: Update Market Data Types
// ============================================================================

/// Request from Keeper to update market data (matches keeper's UpdateMarketDataRequest)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateMarketDataRequest {
    /// Asset IDs (matches keeper's field name)
    pub assets: Vec<u128>,
    /// Correlation ID for tracing
    pub batch_id: String,
}

/// Response to Keeper after processing market data update
#[derive(Debug, Clone, Serialize)]
pub struct UpdateMarketDataResponse {
    /// Status: "success", "partial", or "failed"
    pub status: String,
    /// Number of assets with market data submitted on-chain
    pub assets_updated: usize,
    /// Echo back correlation ID
    pub batch_id: String,
    /// Server timestamp (Unix seconds)
    pub timestamp: u64,
}

// ============================================================================
// Story 0-1 AC6: Refresh Index Quote Types
// ============================================================================

/// Request from Keeper to refresh index quote (AC6)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RefreshQuoteRequest {
    /// Index ID to refresh quote for
    pub index_id: u128,
    /// Correlation ID for tracing
    pub correlation_id: Option<String>,
}

/// Response to Keeper after refreshing index quote (AC6)
#[derive(Debug, Clone, Serialize)]
pub struct RefreshQuoteResponse {
    /// Whether the refresh was successful
    pub success: bool,
    /// Index ID that was refreshed
    pub index_id: u128,
    /// Transaction hash if submitted on-chain
    pub tx_hash: Option<String>,
    /// Correlation ID
    pub correlation_id: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
}
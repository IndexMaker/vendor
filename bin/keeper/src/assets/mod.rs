//! Asset extraction module for Story 2.4
//!
//! Extracts asset IDs from indices via Stewart contract queries.
//! Creates union of all assets across batch indices for Vendor market data.

mod extractor;

pub use extractor::{AssetExtractor, ExtractionResult};

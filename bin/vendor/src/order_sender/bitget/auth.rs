use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha256::digest;
use std::collections::HashMap;

type HmacSha256 = Hmac<sha2::Sha256>;

pub struct BitgetAuth {
    api_key: String,
    api_secret: String,
    passphrase: String,
}

impl BitgetAuth {
    pub fn new(api_key: String, api_secret: String, passphrase: String) -> Self {
        Self {
            api_key,
            api_secret,
            passphrase,
        }
    }

    /// Generate timestamp in milliseconds
    pub fn get_timestamp() -> String {
        Utc::now().timestamp_millis().to_string()
    }

    /// Generate signature for request
    pub fn generate_signature(
        &self,
        timestamp: &str,
        method: &str,
        request_path: &str,
        body: &str,
    ) -> String {
        // Prehash string: timestamp + method + requestPath + body
        let prehash = format!("{}{}{}{}", timestamp, method, request_path, body);

        // HMAC SHA256
        let mut mac = HmacSha256::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(prehash.as_bytes());
        let result = mac.finalize();
        let code_bytes = result.into_bytes();

        // Base64 encode
        general_purpose::STANDARD.encode(code_bytes)
    }

    /// Build authenticated headers
    pub fn build_headers(
        &self,
        timestamp: &str,
        method: &str,
        request_path: &str,
        body: &str,
    ) -> HashMap<String, String> {
        let signature = self.generate_signature(timestamp, method, request_path, body);

        let mut headers = HashMap::new();
        headers.insert("ACCESS-KEY".to_string(), self.api_key.clone());
        headers.insert("ACCESS-SIGN".to_string(), signature);
        headers.insert("ACCESS-TIMESTAMP".to_string(), timestamp.to_string());
        headers.insert("ACCESS-PASSPHRASE".to_string(), self.passphrase.clone());
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("locale".to_string(), "en-US".to_string());

        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_generation() {
        let auth = BitgetAuth::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_pass".to_string(),
        );

        let timestamp = "1234567890000";
        let method = "POST";
        let path = "/api/v2/spot/trade/place-order";
        let body = r#"{"symbol":"BTCUSDT","side":"buy"}"#;

        let sig = auth.generate_signature(timestamp, method, path, body);

        // Signature should be base64 encoded
        assert!(!sig.is_empty());
        assert!(general_purpose::STANDARD.decode(&sig).is_ok());
    }

    #[test]
    fn test_headers_generation() {
        let auth = BitgetAuth::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_pass".to_string(),
        );

        let timestamp = "1234567890000";
        let headers = auth.build_headers(timestamp, "GET", "/api/v2/spot/account/assets", "");

        assert_eq!(headers.get("ACCESS-KEY").unwrap(), "test_key");
        assert_eq!(headers.get("ACCESS-PASSPHRASE").unwrap(), "test_pass");
        assert_eq!(headers.get("ACCESS-TIMESTAMP").unwrap(), timestamp);
        assert!(headers.contains_key("ACCESS-SIGN"));
    }
}
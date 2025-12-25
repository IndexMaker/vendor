use super::messages::{BitgetWsMessage, SubscribeMessage};
use chrono::{DateTime, Utc};
use eyre::{eyre, Result};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde_json;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsReceiver = SplitStream<WsStream>;

pub struct BitgetWebSocket {
    url: String,
    sink: Option<WsSink>,
    receiver: Option<WsReceiver>,
    last_pong: DateTime<Utc>,
}

impl BitgetWebSocket {
    pub async fn connect(url: &str) -> Result<Self> {
        tracing::info!("Connecting to Bitget WebSocket: {}", url);

        let (ws_stream, response) = connect_async(url)
            .await
            .map_err(|e| eyre!("Failed to connect to WebSocket: {}", e))?;

        tracing::info!("WebSocket connected, response: {:?}", response.status());

        let (sink, receiver) = ws_stream.split();

        Ok(Self {
            url: url.to_string(),
            sink: Some(sink),
            receiver: Some(receiver),
            last_pong: Utc::now(),
        })
    }

    pub async fn subscribe(&mut self, symbol: &str) -> Result<()> {
        let subscribe_msg = SubscribeMessage::new(symbol);
        let json = serde_json::to_string(&subscribe_msg)?;

        tracing::debug!("Subscribing to {}: {}", symbol, json);

        self.send_text(&json).await?;
        Ok(())
    }

    pub async fn send_ping(&mut self) -> Result<()> {
        // Bitget uses string "ping", not JSON
        self.send_text("ping").await?;
        tracing::trace!("Sent ping");
        Ok(())
    }

    async fn send_text(&mut self, text: &str) -> Result<()> {
        if let Some(sink) = &mut self.sink {
            sink.send(Message::Text(text.to_string().into()))
                .await
                .map_err(|e| eyre!("Failed to send message: {}", e))?;
            Ok(())
        } else {
            Err(eyre!("WebSocket sink not available"))
        }
    }

    pub async fn next_message(&mut self) -> Result<Option<BitgetWsMessage>> {
        if let Some(receiver) = &mut self.receiver {
            match receiver.next().await {
                Some(Ok(msg)) => self.handle_ws_message(msg),
                Some(Err(e)) => Err(eyre!("WebSocket error: {}", e)),
                None => {
                    tracing::info!("WebSocket stream closed");
                    Ok(None)
                }
            }
        } else {
            Err(eyre!("WebSocket receiver not available"))
        }
    }

    fn handle_ws_message(&mut self, msg: Message) -> Result<Option<BitgetWsMessage>> {
        match msg {
            Message::Text(text) => {
                // Handle "pong" response
                if text.trim() == "pong" {
                    self.last_pong = Utc::now();
                    tracing::trace!("Received pong");
                    return Ok(None);
                }

                // Parse JSON message
                match serde_json::from_str::<BitgetWsMessage>(&text) {
                    Ok(parsed) => {
                        // Check if it's a subscription confirmation
                        if let Some(op) = &parsed.op {
                            if op == "subscribe" {
                                tracing::debug!("Subscription confirmed: {}", text);
                                return Ok(None);
                            }
                        }
                        Ok(Some(parsed))
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse message: {} - Text: {}", e, text);
                        Ok(None)
                    }
                }
            }
            Message::Ping(data) => {
                tracing::trace!("Received WebSocket ping");
                // Tungstenite handles pong automatically
                Ok(None)
            }
            Message::Pong(_) => {
                tracing::trace!("Received WebSocket pong");
                Ok(None)
            }
            Message::Close(frame) => {
                tracing::info!("WebSocket close frame received: {:?}", frame);
                Ok(None)
            }
            Message::Binary(data) => {
                tracing::warn!("Received unexpected binary message: {} bytes", data.len());
                Ok(None)
            }
            Message::Frame(_) => {
                tracing::trace!("Received raw frame");
                Ok(None)
            }
        }
    }

    pub fn last_pong_time(&self) -> DateTime<Utc> {
        self.last_pong
    }

    pub async fn close(mut self) -> Result<()> {
        if let Some(mut sink) = self.sink.take() {
            sink.close()
                .await
                .map_err(|e| eyre!("Failed to close WebSocket: {}", e))?;
        }
        Ok(())
    }
}
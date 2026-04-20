use std::io;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::book::BookSnapshot;
use crate::engine::EngineHandle;
use crate::events::Event;
use crate::types::{AccountId, Cash, InstrumentId, NewOrder, OrderId, Quantity, ReplaceOrder};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayRequest {
    Submit {
        order: NewOrder,
    },
    Cancel {
        order_id: OrderId,
    },
    Replace {
        replace: ReplaceOrder,
    },
    CreditCash {
        account_id: AccountId,
        amount: Cash,
    },
    CreditPosition {
        account_id: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
    },
    Snapshot {
        instrument: InstrumentId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayResponse {
    Events { events: Vec<Event> },
    Snapshot { snapshot: BookSnapshot },
    Error { message: String },
}

pub async fn serve_tcp(addr: &str, engine: EngineHandle) -> io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let engine = Arc::new(engine);

    loop {
        let (stream, _) = listener.accept().await?;
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, engine).await {
                let _ = err;
            }
        });
    }
}

async fn handle_client(stream: TcpStream, engine: Arc<EngineHandle>) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<GatewayRequest>(&line) {
            Ok(request) => handle_request(request, &engine).await,
            Err(err) => GatewayResponse::Error {
                message: format!("invalid request: {err}"),
            },
        };

        let encoded = serde_json::to_string(&response)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        writer.write_all(encoded.as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }

    Ok(())
}

async fn handle_request(request: GatewayRequest, engine: &EngineHandle) -> GatewayResponse {
    match request {
        GatewayRequest::Submit { order } => GatewayResponse::Events {
            events: engine.submit(order).await,
        },
        GatewayRequest::Cancel { order_id } => GatewayResponse::Events {
            events: engine.cancel(order_id).await,
        },
        GatewayRequest::Replace { replace } => GatewayResponse::Events {
            events: engine.replace(replace).await,
        },
        GatewayRequest::CreditCash { account_id, amount } => GatewayResponse::Events {
            events: engine.credit_cash(account_id, amount).await,
        },
        GatewayRequest::CreditPosition {
            account_id,
            instrument,
            quantity,
        } => GatewayResponse::Events {
            events: engine
                .credit_position(account_id, instrument, quantity)
                .await,
        },
        GatewayRequest::Snapshot { instrument } => match engine.snapshot(instrument).await {
            Some(snapshot) => GatewayResponse::Snapshot { snapshot },
            None => GatewayResponse::Error {
                message: "engine unavailable".to_string(),
            },
        },
    }
}

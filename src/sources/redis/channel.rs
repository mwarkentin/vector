use futures_util::StreamExt;
use snafu::{ResultExt, Snafu};
use vector_common::internal_event::{BytesReceived, Registered};
use vector_core::config::LogNamespace;

use crate::{
    codecs,
    config::SourceContext,
    internal_events::RedisReceiveEventError,
    sources::{
        redis::{handle_line, ConnectionInfo},
        Source,
    },
};

#[derive(Debug, Snafu)]
enum BuildError {
    #[snafu(display("Failed to create connection: {}", source))]
    Connection { source: redis::RedisError },
    #[snafu(display("Failed to subscribe to channel: {}", source))]
    Subscribe { source: redis::RedisError },
}

pub async fn subscribe(
    client: redis::Client,
    connection_info: ConnectionInfo,
    bytes_received: Registered<BytesReceived>,
    key: String,
    redis_key: Option<String>,
    decoder: codecs::Decoder,
    cx: SourceContext,
    log_namespace: LogNamespace,
) -> crate::Result<Source> {
    let conn = client
        .get_async_connection()
        .await
        .context(ConnectionSnafu {})?;

    trace!(endpoint = %connection_info.endpoint.as_str(), "Connected.");

    let mut pubsub_conn = conn.into_pubsub();
    pubsub_conn
        .subscribe(&key)
        .await
        .context(SubscribeSnafu {})?;
    trace!(endpoint = %connection_info.endpoint.as_str(), channel = %key, "Subscribed to channel.");

    Ok(Box::pin(async move {
        let shutdown = cx.shutdown;
        let mut tx = cx.out;
        let mut pubsub_stream = pubsub_conn.on_message().take_until(shutdown);
        while let Some(msg) = pubsub_stream.next().await {
            match msg.get_payload::<String>() {
                Ok(line) => {
                    if let Err(()) = handle_line(
                        line,
                        &key,
                        redis_key.as_deref(),
                        decoder.clone(),
                        &bytes_received,
                        &mut tx,
                        log_namespace,
                    )
                    .await
                    {
                        break;
                    }
                }
                Err(error) => emit!(RedisReceiveEventError::from(error)),
            }
        }
        Ok(())
    }))
}

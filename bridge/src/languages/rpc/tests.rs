use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

type ServerReader = BufReader<ReadHalf<DuplexStream>>;
fn connection(
    capacity: usize,
) -> (
    Client,
    mpsc::Receiver<ServerMessage>,
    ServerReader,
    WriteHalf<DuplexStream>,
) {
    let (client, server) = tokio::io::duplex(capacity);
    let (input, output) = tokio::io::split(client);
    let (server_input, server_output) = tokio::io::split(server);
    let (client, events) = Client::connect(output, BufReader::new(input));
    (client, events, BufReader::new(server_input), server_output)
}

#[tokio::test]
async fn concurrent_responses_and_server_requests_do_not_block_each_other() {
    let (client, mut events, mut input, mut output) = connection(4096);
    let a = client.clone();
    let b = client.clone();
    let first =
        tokio::spawn(async move { a.request("a", json!({}), Duration::from_secs(2)).await });
    let second =
        tokio::spawn(async move { b.request("b", json!({}), Duration::from_secs(2)).await });
    let a = framing::read(&mut input).await.unwrap();
    let b = framing::read(&mut input).await.unwrap();
    framing::write(&mut output, &json!({"jsonrpc":"2.0","id":"configuration-1","method":"workspace/configuration","params":{"items":[]}})).await.unwrap();
    let event = events.recv().await.unwrap();
    assert_eq!(event.method, "workspace/configuration");
    client
        .send(json!({"jsonrpc":"2.0","id":event.id,"result":[]}))
        .await
        .unwrap();
    let answer = framing::read(&mut input).await.unwrap();
    assert_eq!(answer["id"], "configuration-1");
    for message in [b, a] {
        framing::write(
            &mut output,
            &json!({"jsonrpc":"2.0","id":message["id"],"result":message["method"]}),
        )
        .await
        .unwrap();
    }
    assert_eq!(first.await.unwrap().unwrap(), "a");
    assert_eq!(second.await.unwrap().unwrap(), "b");
}

#[tokio::test]
async fn cancellation_finishes_the_frame_and_cannot_satisfy_a_new_request() {
    let (client, _events, mut input, mut output) = connection(32);
    let requesting = client.clone();
    let task = tokio::spawn(async move {
        requesting
            .request(
                "large",
                json!({"source":"水😀".repeat(10_000)}),
                Duration::from_secs(2),
            )
            .await
    });
    // The pipe is full before the body is written. Cancel the owner while the
    // dedicated writer still has most of its frame left to deliver.
    assert!(!input.fill_buf().await.unwrap().is_empty());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let abandoned = framing::read(&mut input).await.unwrap();
    assert_eq!(abandoned["method"], "large");
    assert_eq!(
        abandoned["params"]["source"].as_str().unwrap().len(),
        70_000
    );
    let cancelled = framing::read(&mut input).await.unwrap();
    assert_eq!(cancelled["method"], "$/cancelRequest");
    assert_eq!(cancelled["params"]["id"], abandoned["id"]);
    framing::write(
        &mut output,
        &json!({"jsonrpc":"2.0","id":abandoned["id"],"result":"late"}),
    )
    .await
    .unwrap();
    let requesting = client.clone();
    let current = tokio::spawn(async move {
        requesting
            .request("current", json!({}), Duration::from_secs(2))
            .await
    });
    let request = framing::read(&mut input).await.unwrap();
    assert_ne!(request["id"], abandoned["id"]);
    framing::write(
        &mut output,
        &json!({"jsonrpc":"2.0","id":request["id"],"result":"current"}),
    )
    .await
    .unwrap();
    assert_eq!(current.await.unwrap().unwrap(), "current");
}

#[tokio::test]
async fn remote_errors_remain_distinct_from_pipe_failures() {
    let (client, _events, mut input, mut output) = connection(4096);
    let requesting = client.clone();
    let task = tokio::spawn(async move {
        requesting
            .request("unsupported", json!({}), Duration::from_secs(2))
            .await
    });
    let request = framing::read(&mut input).await.unwrap();
    framing::write(&mut output, &json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"not supported"}})).await.unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Err(Error::Remote { code: -32601, .. })
    ));
    assert!(!client.is_closed());
    let requesting = client.clone();
    let pending = tokio::spawn(async move {
        requesting
            .request("waiting", json!({}), Duration::from_secs(2))
            .await
    });
    framing::read(&mut input).await.unwrap();
    output.shutdown().await.unwrap();
    assert!(matches!(pending.await.unwrap(), Err(Error::Transport(_))));
    assert!(client.is_closed());
}

#[tokio::test]
async fn bounded_event_queue_rejects_unconsumed_server_floods() {
    let (client, _events, _input, mut output) = connection(4096);
    for _ in 0..=MAX_EVENTS {
        framing::write(
            &mut output,
            &json!({"jsonrpc":"2.0","method":"notification"}),
        )
        .await
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while !client.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        client
            .request("after_flood", json!({}), Duration::from_secs(2))
            .await
            .is_err()
    );
}

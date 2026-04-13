use std::sync::Arc;

use serde_json::{json, Value};

use orca::daemon::server::{IpcClient, IpcServer};
use orca::protocol::{RpcError, RpcRequest, RpcResponse, METHOD_NOT_FOUND};

#[test]
fn test_rpc_request_serialization() {
    let request = RpcRequest::new("task.create", json!({"title": "hello"}));

    let serialized = serde_json::to_string(&request).expect("serialize");
    let deserialized: RpcRequest = serde_json::from_str(&serialized).expect("deserialize");

    assert_eq!(deserialized.jsonrpc, "2.0");
    assert_eq!(deserialized.method, "task.create");
    assert_eq!(deserialized.id, json!(1));
    assert_eq!(deserialized.params, json!({"title": "hello"}));
}

#[test]
fn test_rpc_success_response() {
    let response = RpcResponse::success(json!(1), json!({"status": "ok"}));

    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(1));
    assert!(response.result.is_some());
    assert!(response.error.is_none());

    // Verify serialization omits null error field
    let serialized = serde_json::to_string(&response).expect("serialize");
    let parsed: Value = serde_json::from_str(&serialized).expect("parse");
    assert_eq!(parsed.get("result").unwrap(), &json!({"status": "ok"}));
    assert!(parsed.get("error").is_none());
}

#[test]
fn test_rpc_error_response() {
    let error = RpcError {
        code: METHOD_NOT_FOUND,
        message: "method not found".to_string(),
        data: None,
    };
    let response = RpcResponse::error(json!(42), error);

    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(42));
    assert!(response.result.is_none());
    assert!(response.error.is_some());

    // Verify serialization omits null result field and contains error
    let serialized = serde_json::to_string(&response).expect("serialize");
    let parsed: Value = serde_json::from_str(&serialized).expect("parse");
    assert!(parsed.get("result").is_none());

    let err_obj = parsed.get("error").expect("error field present");
    assert_eq!(err_obj.get("code").unwrap(), METHOD_NOT_FOUND);
    assert_eq!(err_obj.get("message").unwrap(), "method not found");
}

#[tokio::test]
async fn test_ipc_roundtrip() {
    let tmp_dir = tempfile::tempdir().expect("create tempdir");
    let socket_path = tmp_dir.path().join("test.sock");

    // Handler: echo params back as result, or error for unknown methods
    let handler = Arc::new(|req: RpcRequest| -> RpcResponse {
        match req.method.as_str() {
            "echo" => RpcResponse::success(req.id, req.params),
            _ => RpcResponse::error(
                req.id,
                RpcError {
                    code: METHOD_NOT_FOUND,
                    message: format!("unknown method: {}", req.method),
                    data: None,
                },
            ),
        }
    });

    let server = IpcServer::bind(&socket_path, handler).expect("bind");
    let server_handle = tokio::spawn(async move {
        let _ = server.run().await;
    });

    // Give the server a moment to start accepting
    tokio::task::yield_now().await;

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");

    // Test successful call
    let req = RpcRequest::new("echo", json!({"msg": "hello orca"}));
    let resp = client.call(&req).await.expect("call echo");
    assert_eq!(resp.id, json!(1));
    assert!(resp.error.is_none());
    assert_eq!(resp.result.unwrap(), json!({"msg": "hello orca"}));

    // Test error call
    let req2 = RpcRequest::new("nonexistent", json!(null));
    let resp2 = client.call(&req2).await.expect("call nonexistent");
    assert!(resp2.result.is_none());
    let err = resp2.error.unwrap();
    assert_eq!(err.code, METHOD_NOT_FOUND);
    assert!(err.message.contains("nonexistent"));

    // Clean up
    server_handle.abort();
}

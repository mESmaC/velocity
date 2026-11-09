//! The requests and notifications the agent sends Velocity. Handlers run on the
//! connection's dispatch loop, so anything that waits on I/O is spawned off it.

use std::sync::Arc;

use agent_client_protocol::v1::schema::{
    CreateTerminalRequest, KillTerminalRequest, KillTerminalResponse, ReadTextFileRequest,
    ReleaseTerminalRequest, ReleaseTerminalResponse, RequestPermissionRequest, SessionNotification,
    TerminalOutputRequest, WaitForTerminalExitRequest, WaitForTerminalExitResponse,
    WriteTextFileRequest,
};
use agent_client_protocol::v1::{Builder, ConnectionTo, Responder};
use agent_client_protocol::{
    Agent, Client, HandleDispatchFrom, JsonRpcResponse, on_receive_notification, on_receive_request,
};

use super::{AgentEvent, ClientError, Shared, answer_permission, files};

pub fn register(
    builder: Builder<Client>,
    shared: &Arc<Shared>,
) -> Builder<Client, impl HandleDispatchFrom<Agent>> {
    let builder = with_session_handlers(builder, shared);
    let builder = with_file_handlers(builder, shared);
    let builder = with_terminal_handlers(builder, shared);
    with_terminal_lifetime_handlers(builder, shared)
}

fn respond<Response: JsonRpcResponse>(
    responder: Responder<Response>,
    result: Result<Response, ClientError>,
) -> Result<(), agent_client_protocol::Error> {
    match result {
        Ok(response) => responder.respond(response),
        Err(error) => responder.respond_with_error(error.into()),
    }
}

fn with_session_handlers(
    builder: Builder<Client, impl HandleDispatchFrom<Agent>>,
    shared: &Arc<Shared>,
) -> Builder<Client, impl HandleDispatchFrom<Agent>> {
    let notified = Arc::clone(shared);
    builder
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                responder.respond(answer_permission(&request))
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                notified.record(notification.update);
                Ok(())
            },
            on_receive_notification!(),
        )
}

fn with_file_handlers(
    builder: Builder<Client, impl HandleDispatchFrom<Agent>>,
    shared: &Arc<Shared>,
) -> Builder<Client, impl HandleDispatchFrom<Agent>> {
    let reader = Arc::clone(shared);
    let writer = Arc::clone(shared);
    builder
        .on_receive_request(
            async move |request: ReadTextFileRequest,
                        responder,
                        connection: ConnectionTo<Agent>| {
                let shared = Arc::clone(&reader);
                connection.spawn(async move {
                    respond(
                        responder,
                        files::read_text_file(&shared.worktree, request).await,
                    )
                })
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WriteTextFileRequest,
                        responder,
                        connection: ConnectionTo<Agent>| {
                let shared = Arc::clone(&writer);
                connection.spawn(async move {
                    let written = files::write_text_file(&shared.worktree, request).await;
                    let response = written.map(|(response, path)| {
                        shared.emit(AgentEvent::FileWritten { path });
                        response
                    });
                    respond(responder, response)
                })
            },
            on_receive_request!(),
        )
}

fn with_terminal_handlers(
    builder: Builder<Client, impl HandleDispatchFrom<Agent>>,
    shared: &Arc<Shared>,
) -> Builder<Client, impl HandleDispatchFrom<Agent>> {
    let creator = Arc::clone(shared);
    let reader = Arc::clone(shared);
    let waiter = Arc::clone(shared);
    builder
        .on_receive_request(
            async move |request: CreateTerminalRequest, responder, _connection| {
                respond(responder, creator.terminals.create(request))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: TerminalOutputRequest, responder, _connection| {
                respond(responder, reader.terminals.output(&request.terminal_id.0))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WaitForTerminalExitRequest,
                        responder,
                        connection: ConnectionTo<Agent>| {
                let shared = Arc::clone(&waiter);
                connection.spawn(async move {
                    let status = shared.terminals.wait_for_exit(&request.terminal_id.0).await;
                    respond(responder, status.map(WaitForTerminalExitResponse::new))
                })
            },
            on_receive_request!(),
        )
}

fn with_terminal_lifetime_handlers(
    builder: Builder<Client, impl HandleDispatchFrom<Agent>>,
    shared: &Arc<Shared>,
) -> Builder<Client, impl HandleDispatchFrom<Agent>> {
    let killer = Arc::clone(shared);
    let releaser = Arc::clone(shared);
    builder
        .on_receive_request(
            async move |request: KillTerminalRequest, responder, _connection| {
                let killed = killer.terminals.kill(&request.terminal_id.0);
                respond(responder, killed.map(|()| KillTerminalResponse::new()))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReleaseTerminalRequest, responder, _connection| {
                let released = releaser.terminals.release(&request.terminal_id.0);
                respond(responder, released.map(|()| ReleaseTerminalResponse::new()))
            },
            on_receive_request!(),
        )
}

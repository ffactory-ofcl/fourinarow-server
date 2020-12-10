use super::client_state::*;
use super::msg::*;
use super::ClientConnection;

use actix::*;

use std::time::{Duration, Instant};

const QUEUE_CHECK_INTERVAL_MS: u64 = 500;
const QUEUE_RESEND_TIMEOUT_MS: u128 = 5000; // TODO change back to 1000

#[derive(Default)]
struct ReliabilityLayer {
    player_msg_index: usize, // Last successfully received message
    player_msg_q: Vec<QueuedMessage<PlayerMessage>>,
    server_msg_index: usize, // Last sent message
    server_msg_q: Vec<QueuedMessage<ServerMessage>>,
}

#[derive(Debug)]
struct QueuedMessage<T> {
    sent: Instant,
    id: usize,
    msg: T,
}

impl QueuedMessage<ServerMessage> {
    fn to_packet(self) -> ReliablePacket<ServerMessage> {
        ReliablePacket::Msg(self.id, self.msg)
    }
}

enum ClientConnectionConnectionState {
    Connected(Addr<ClientConnection>),
    Disconnected,
}

pub struct ClientAdapter {
    client_connection: ClientConnectionConnectionState,
    client_state: Addr<ClientState>,
    reliability_layer: ReliabilityLayer,
}

impl ClientAdapter {
    pub fn new(
        client_connection: Addr<ClientConnection>,
        client_state_addr: Addr<ClientState>,
    ) -> ClientAdapter {
        ClientAdapter {
            client_connection: ClientConnectionConnectionState::Connected(client_connection),
            client_state: client_state_addr,
            reliability_layer: ReliabilityLayer::default(),
        }
    }

    fn resend_queued_interval(&self, ctx: &mut Context<Self>) {
        ctx.run_interval(
            Duration::from_millis(QUEUE_CHECK_INTERVAL_MS),
            |act, ctx| {
                let messages: Vec<QueuedMessage<ServerMessage>> =
                    act.reliability_layer.server_msg_q.drain(..).collect();

                for queued_msg in messages {
                    if queued_msg.sent.elapsed().as_millis() >= QUEUE_RESEND_TIMEOUT_MS {
                        ctx.notify(queued_msg.to_packet());
                    } else {
                        act.reliability_layer.server_msg_q.push(queued_msg);
                    }
                }
            },
        );
    }

    fn received_reliable_pkt(
        &mut self,
        msg: ReliablePacket<PlayerMessage>,
        ctx: &mut Context<Self>,
    ) {
        match msg {
            // ReliableMessage::Syn(starting_id) => {
            //     self.reliability_layer.client_last_msg_id = Some(starting_id);
            // }
            ReliablePacket::Ack(id) => {
                let maybe_rmsg = self
                    .reliability_layer
                    .server_msg_q
                    .iter()
                    .position(|rmsg| rmsg.id == id);
                if let Some(rmsg_index) = maybe_rmsg {
                    self.reliability_layer.server_msg_q.remove(rmsg_index);
                }

                // } else {
                //     ctx.notify(ServerMessage::Error(Some(SrvMsgError::InvalidMessage)));
                //     ctx.stop();
                // }
            }
            ReliablePacket::Msg(id, player_msg) => {
                let expected_id = self.reliability_layer.player_msg_index + 1;
                if id == expected_id {
                    // We got the expected message
                    self.reliability_layer.player_msg_index = expected_id;
                    self.forward_message(player_msg, ctx);
                    self.ack_message(id, ctx);

                    // Process queued messages that might have been waiting
                    self.process_queue(ctx);
                } else if id > expected_id {
                    // A message between this and the last one was lost. Queue this one and wait for client to resend it
                    self.queue_message(id, player_msg);
                    // Ack (duplicate) last successful message
                    self.ack_message(self.reliability_layer.player_msg_index, ctx);
                } else {
                    // Client re-sent already known message -> maybe ack got lost -> ack but don't process
                    self.ack_message(id, ctx);
                }
            }
        }
    }

    /// Processes all player messages in the queue that can be ordered
    /// If player_msg_index is 2 and the queue contains [4, 5, 3, 7] all except 7 will be processed
    fn process_queue(&mut self, ctx: &mut Context<Self>) {
        loop {
            let mut added = false;
            let messages: Vec<_> = self.reliability_layer.player_msg_q.drain(..).collect();
            for queued_message in messages {
                let expected_id = self.reliability_layer.player_msg_index + 1;
                if queued_message.id == expected_id {
                    self.reliability_layer.player_msg_index = expected_id;
                    // We got the expected message. Process queued messages that might have been waiting
                    self.forward_message(queued_message.msg, ctx);
                    self.ack_message(queued_message.id, ctx);
                    added = true;
                } else {
                    self.reliability_layer.player_msg_q.push(queued_message);
                }
            }
            if !added {
                break;
            }
        }
    }

    fn ack_message(&mut self, id: usize, ctx: &mut Context<Self>) {
        ctx.notify(ReliablePacket::Ack(id));
    }

    fn queue_message(&mut self, id: usize, msg: PlayerMessage) {
        self.reliability_layer.player_msg_q.push(QueuedMessage {
            sent: Instant::now(),
            id,
            msg: msg.clone(),
        });
    }

    fn forward_message(&mut self, msg: PlayerMessage, ctx: &mut Context<Self>) {
        self.client_state
            .send(msg)
            .into_actor(self)
            .then(|msg_res, _, ctx: &mut Context<Self>| {
                if msg_res.is_err() {
                    ctx.notify(ServerMessage::Error(Some(SrvMsgError::Internal)));
                    println!("ClientAdapter: Failed to send message to client state");
                    ctx.stop();
                }
                fut::ready(())
            })
            .wait(ctx);
    }
}

pub struct ClientMsgString(pub String);

impl Message for ClientMsgString {
    type Result = ();
}

impl Into<String> for ClientMsgString {
    fn into(self) -> String {
        self.0
    }
}

impl Handler<ClientMsgString> for ClientAdapter {
    type Result = ();

    fn handle(&mut self, msg: ClientMsgString, ctx: &mut Self::Context) -> Self::Result {
        match ReliablePacket::parse(&msg.0) {
            Ok(msg) => self.received_reliable_pkt(msg, ctx),
            Err(reliability_err) => {
                // TODO!
                println!("   ## -> Invalid message (error: {:?})", reliability_err);
                ctx.notify(ServerMessage::Error(None));
            }
        }
    }
}

impl Handler<ServerMessage> for ClientAdapter {
    type Result = Result<(), ()>;
    fn handle(&mut self, msg: ServerMessage, ctx: &mut Self::Context) -> Self::Result {
        self.reliability_layer.server_msg_index += 1;
        ctx.notify(ReliablePacket::Msg(
            self.reliability_layer.server_msg_index,
            msg,
        ));
        Ok(())
    }
}

impl Handler<ReliablePacket<ServerMessage>> for ClientAdapter {
    type Result = ();
    fn handle(&mut self, msg: ReliablePacket<ServerMessage>, _ctx: &mut Self::Context) {
        if let ReliablePacket::Msg(id, server_msg) = msg.clone() {
            self.reliability_layer.server_msg_q.push(QueuedMessage {
                id,
                msg: server_msg.clone(),
                sent: Instant::now(),
            });
        }

        let msg_str = msg.serialize();
        if let ClientConnectionConnectionState::Connected(client_connection) =
            &self.client_connection
        {
            client_connection.do_send(ClientMsgString(msg_str));
        }
    }
}

impl Handler<ClientStateMessage> for ClientAdapter {
    type Result = Result<(), ()>;
    fn handle(&mut self, msg: ClientStateMessage, _: &mut Self::Context) -> Self::Result {
        self.client_state.do_send(msg);
        Ok(())
    }
}

pub enum ClientAdapterMsg {
    Connect(Addr<ClientConnection>),
    Disconnect,
    Close,
}
impl Message for ClientAdapterMsg {
    type Result = ();
}
impl Handler<ClientAdapterMsg> for ClientAdapter {
    type Result = ();
    fn handle(&mut self, msg: ClientAdapterMsg, ctx: &mut Self::Context) -> Self::Result {
        match msg {
            ClientAdapterMsg::Connect(client_connection_addr) => {
                self.client_connection =
                    ClientConnectionConnectionState::Connected(client_connection_addr);
            }
            ClientAdapterMsg::Disconnect => {
                self.client_connection = ClientConnectionConnectionState::Disconnected;
            }
            ClientAdapterMsg::Close => {
                ctx.stop();
            }
        }
    }
}

impl Actor for ClientAdapter {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.resend_queued_interval(ctx);
        self.client_state
            .do_send(ClientStateMessage::BackLink(ctx.address()));

        // self.connection_mgr
        //     .do_send(ConnectionManagerMsg::Hello(self.client_state.clone()));
    }

    fn stopping(&mut self, _ctx: &mut Self::Context) -> Running {
        self.client_state.do_send(ClientStateMessage::Close);
        Running::Stop
    }
}

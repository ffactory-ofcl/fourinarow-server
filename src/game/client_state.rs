use super::lobby_mgr::{self, *};
use super::msg::*;
use super::{
    client_adapter::{ClientAdapter, ClientAdapterMsg},
    connection_mgr::ConnectionManager,
};
use super::{connection_mgr::ConnectionManagerMsg, game_info::*};
use super::{connection_mgr::WSSessionToken, lobby::*};
use crate::{
    api::users::{
        user::PublicUserMe,
        user_mgr::{self, UserManager},
    },
    logging::*,
};

use actix::*;

pub struct ClientState {
    id: WSSessionToken,
    lobby_mgr: Addr<LobbyManager>,
    user_mgr: Addr<UserManager>,
    _logger: Addr<Logger>,
    connection_mgr: Addr<ConnectionManager>,
    backlinked_state: BacklinkState,
    conn_state: ClientConnState,
    maybe_user_info: Option<PublicUserMe>,
}

pub enum ClientConnState {
    Idle,
    InLobby(Player, Addr<Lobby>),
}

impl ClientState {
    pub fn new(
        id: WSSessionToken,
        lobby_mgr: Addr<LobbyManager>,
        user_mgr: Addr<UserManager>,
        _logger: Addr<Logger>,
        connection_mgr: Addr<ConnectionManager>,
    ) -> ClientState {
        ClientState {
            id,
            lobby_mgr,
            user_mgr,
            _logger,
            connection_mgr,
            backlinked_state: BacklinkState::Waiting,
            conn_state: ClientConnState::Idle,
            maybe_user_info: None,
        }
    }
}

enum BacklinkState {
    Waiting,
    Linked(Addr<ClientAdapter>),
}

pub enum ClientStateMessage {
    BackLink(Addr<ClientAdapter>),
    Reset,
    Close, // Triggered by client timeout or disconnect
    BattleReqJoinLobby(Addr<Lobby>),
    CurrentServerState(usize, bool, bool), // connected players, someone wants to play, [internal: was requeued]
}

impl Handler<ClientStateMessage> for ClientState {
    type Result = Result<(), ()>;
    fn handle(&mut self, msg: ClientStateMessage, ctx: &mut Self::Context) -> Self::Result {
        use ClientStateMessage::*;
        match msg {
            BackLink(addr) => {
                if let BacklinkState::Waiting = self.backlinked_state {
                    self.backlinked_state = BacklinkState::Linked(addr);
                }
            }
            Close => {
                if let ClientConnState::InLobby(player, lobby_addr) = &self.conn_state {
                    lobby_addr.do_send(ClientLobbyMessageNamed {
                        sender: *player,
                        msg: ClientLobbyMessage::PlayerLeaving {
                            reason: PlayerLeaveReason::Disconnect,
                        },
                    });
                    self.conn_state = ClientConnState::Idle;
                }
                ctx.stop();
            }
            Reset => {
                self.conn_state = ClientConnState::Idle;
            }
            BattleReqJoinLobby(addr) => {
                if let BacklinkState::Linked(ref client_conn_addr) = self.backlinked_state {
                    self.conn_state = ClientConnState::InLobby(Player::One, addr);
                    client_conn_addr.do_send(ServerMessage::Okay);
                }
            }
            CurrentServerState(connected_players, player_waiting, requequed) => {
                if let BacklinkState::Linked(ref client_conn_addr) = self.backlinked_state {
                    client_conn_addr.do_send(ServerMessage::CurrentServerState(
                        connected_players,
                        player_waiting,
                    ));
                } else if requequed {
                    // Error: message was already requeued once using notify()
                    // Don't do it again and disconnect client
                    ctx.stop()
                } else {
                    ctx.notify(CurrentServerState(connected_players, player_waiting, true));
                }
            }
        }
        Ok(())
    }
}

impl Handler<PlayerMessage> for ClientState {
    type Result = Result<(), ()>;
    fn handle(&mut self, msg: PlayerMessage, ctx: &mut Self::Context) -> Self::Result {
        let ok = Ok(());
        let err = Err(());
        use PlayerMessage::*;

        if let BacklinkState::Linked(ref client_adapter_addr) = self.backlinked_state {
            match msg {
                Ping => {
                    client_adapter_addr.do_send(ServerMessage::Pong);
                    ok
                }
                PlaceChip(column) => {
                    if let ClientConnState::InLobby(player, lobby_addr) = &self.conn_state {
                        lobby_addr.do_send(ClientLobbyMessageNamed {
                            sender: *player,
                            msg: ClientLobbyMessage::PlaceChip(column),
                        });
                        ok
                    } else {
                        client_adapter_addr
                            .do_send(ServerMessage::Error(Some(SrvMsgError::NotInLobby)));
                        err
                    }
                }
                PlayAgainRequest => {
                    if let ClientConnState::InLobby(player, lobby_addr) = &self.conn_state {
                        lobby_addr.do_send(ClientLobbyMessageNamed {
                            sender: *player,
                            msg: ClientLobbyMessage::PlayAgainRequest,
                        });
                        ok
                    } else {
                        client_adapter_addr
                            .do_send(ServerMessage::Error(Some(SrvMsgError::NotInLobby)));
                        err
                    }
                }
                Leaving => {
                    match &self.conn_state {
                        ClientConnState::InLobby(player, lobby_addr) => {
                            // println!("ClientState: forwarding Leave message to lobby.");
                            if lobby_addr
                                .try_send(ClientLobbyMessageNamed {
                                    sender: *player,
                                    msg: ClientLobbyMessage::PlayerLeaving {
                                        reason: PlayerLeaveReason::Leave,
                                    },
                                })
                                .is_err()
                            {
                                // client_conn_addr
                                //     .do_send(ServerMessage::Error(Some(SrvMsgError::Internal)));
                                // TODO: Lobby is dead. Send okay or error here?
                            }
                        }
                        ClientConnState::Idle => {
                            client_adapter_addr
                                .do_send(ServerMessage::Error(Some(SrvMsgError::NotInLobby)));
                        }
                    }
                    self.conn_state = ClientConnState::Idle;
                    ok
                }
                LobbyRequest(kind) => {
                    if let ClientConnState::Idle = &self.conn_state {
                        let client_conn_addr = client_adapter_addr.clone();
                        self.lobby_mgr
                            .send(lobby_mgr::LobbyRequest::NewLobby(
                                client_conn_addr.clone(),
                                self.maybe_user_info.clone().map(|u| u.id),
                                kind,
                            ))
                            .into_actor(self)
                            .then(move |res, act, _ctx| {
                                if let Ok(lobbyreq_resp_res) = res {
                                    if let Ok(lobbyreq_resp) = lobbyreq_resp_res {
                                        act.conn_state = ClientConnState::InLobby(
                                            lobbyreq_resp.player,
                                            lobbyreq_resp.lobby_addr,
                                        );
                                    }
                                } else {
                                    client_conn_addr
                                        .do_send(ServerMessage::Error(Some(SrvMsgError::Internal)));
                                }
                                fut::ready(())
                            })
                            .wait(ctx);
                        ok
                    } else {
                        client_adapter_addr
                            .do_send(ServerMessage::Error(Some(SrvMsgError::AlreadyInLobby)));
                        err
                    }
                }
                LobbyJoin(id) => {
                    if let ClientConnState::Idle = &self.conn_state {
                        self.lobby_mgr
                            .send(lobby_mgr::LobbyRequest::JoinLobby(
                                id,
                                client_adapter_addr.clone(),
                                self.maybe_user_info.clone().map(|u| u.id),
                                LobbyKind::Private,
                            ))
                            .into_actor(self)
                            .map(|res, act, _ctx| {
                                if let Ok(lobbyreq_resp_res) = res {
                                    if let Ok(lobbyreq_resp) = lobbyreq_resp_res {
                                        act.conn_state = ClientConnState::InLobby(
                                            lobbyreq_resp.player,
                                            lobbyreq_resp.lobby_addr,
                                        );
                                    }
                                }
                            })
                            .wait(ctx);
                        ok
                    } else {
                        client_adapter_addr
                            .do_send(ServerMessage::Error(Some(SrvMsgError::AlreadyInLobby)));
                        err
                    }
                }
                Login(session_token) => {
                    if let ClientConnState::InLobby(_, _) = self.conn_state {
                        client_adapter_addr
                            .do_send(ServerMessage::Error(Some(SrvMsgError::AlreadyInLobby)));
                        return err;
                    }
                    if let Some(user_info) = self.maybe_user_info.clone() {
                        self.user_mgr
                            .do_send(user_mgr::msg::IntUserMgrMsg::StopPlaying(
                                user_info.id,
                                client_adapter_addr.clone(),
                            ));
                    }
                    let client_conn_addr = client_adapter_addr.clone();
                    self.user_mgr
                        .send(user_mgr::msg::StartPlaying {
                            session_token,
                            addr: client_conn_addr.clone(),
                        })
                        .into_actor(self)
                        .then(move |res, act, _| {
                            if let Ok(maybe_id) = res {
                                match maybe_id {
                                    Ok(user) => {
                                        println!("Start playing! user: {:?}", user);
                                        act.maybe_user_info = Some(user);
                                        // act.user_mgr.do_send(IntUserMgrMsg::StartPlaying(id));
                                        client_conn_addr.do_send(ServerMessage::Okay);
                                    }
                                    Err(srv_msg_err) => {
                                        client_conn_addr
                                            .do_send(ServerMessage::Error(Some(srv_msg_err)));
                                    }
                                }
                            } else {
                                client_conn_addr
                                    .do_send(ServerMessage::Error(Some(SrvMsgError::Internal)));
                            }
                            fut::ready(())
                        })
                        .wait(ctx);
                    ok
                }

                Logout => {
                    if let Some(user_info) = self.maybe_user_info.clone() {
                        self.user_mgr
                            .do_send(user_mgr::msg::IntUserMgrMsg::StopPlaying(
                                user_info.id,
                                client_adapter_addr.clone(),
                            ));
                    }
                    ok
                }

                BattleReq(friend_id) => {
                    if let ClientConnState::Idle = &self.conn_state {
                        if let Some(user_info) = self.maybe_user_info.clone() {
                            self.user_mgr.do_send(user_mgr::msg::BattleReq {
                                sender: (client_adapter_addr.clone(), user_info.id),
                                receiver: friend_id,
                            });
                            ok
                        } else {
                            client_adapter_addr
                                .do_send(ServerMessage::Error(Some(SrvMsgError::NotLoggedIn)));
                            err
                        }
                    } else {
                        client_adapter_addr
                            .do_send(ServerMessage::Error(Some(SrvMsgError::AlreadyInLobby)));
                        err
                    }
                }
                ChatMessage(msg) => {
                    if let ClientConnState::InLobby(player, lobby_addr) = &self.conn_state {
                        let username = if let Some(user_info) = self.maybe_user_info.clone() {
                            Some(user_info.username)
                        } else {
                            None
                        };
                        lobby_addr.do_send(ClientLobbyMessageNamed {
                            sender: *player,
                            msg: ClientLobbyMessage::ChatMessage(msg, username),
                        });
                    } else {
                        self.connection_mgr
                            .do_send(ConnectionManagerMsg::ChatMessage(self.id.clone(), msg));
                    }
                    client_adapter_addr.do_send(ServerMessage::Okay);
                    ok
                }
                ChatRead => {
                    if let ClientConnState::InLobby(player, lobby_addr) = &self.conn_state {
                        lobby_addr.do_send(ClientLobbyMessageNamed {
                            sender: *player,
                            msg: ClientLobbyMessage::ChatRead,
                        });
                    } else {
                        self.connection_mgr
                            .do_send(ConnectionManagerMsg::ChatRead(self.id.clone()));
                    }
                    ok
                }
            }
        } else {
            err
        }
    }
}

impl Handler<ServerMessage> for ClientState {
    type Result = Result<(), ()>;

    fn handle(&mut self, msg: ServerMessage, _: &mut Self::Context) -> Self::Result {
        if let BacklinkState::Linked(ref client_conn_addr) = self.backlinked_state {
            client_conn_addr.do_send(msg);
            Ok(())
        } else {
            Err(())
        }
    }
}

impl Message for ClientStateMessage {
    type Result = Result<(), ()>;
}

impl Actor for ClientState {
    type Context = Context<Self>;

    fn stopping(&mut self, _ctx: &mut Self::Context) -> Running {
        // println!("ClientState: Stopping");
        if let BacklinkState::Linked(client_adapter_addr) = &self.backlinked_state {
            if let Some(user_info) = self.maybe_user_info.clone() {
                self.user_mgr
                    .do_send(user_mgr::msg::IntUserMgrMsg::StopPlaying(
                        user_info.id,
                        client_adapter_addr.clone(),
                    ));
            }
        }
        if let BacklinkState::Linked(client_adapter) = &self.backlinked_state {
            client_adapter.do_send(ClientAdapterMsg::Close);
        }
        Running::Stop
    }
}

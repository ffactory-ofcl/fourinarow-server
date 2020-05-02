pub mod user;
pub mod user_manager;

use super::ApiResponse;
use actix::{Addr, MailboxError};
use actix_web::*;
use serde::{Deserialize, Serialize};
use HttpResponse as HR;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg
        // .route("", web::get().to(users))
        .route("", web::get().to(search_user))
        .service(
            web::scope("/me")
                .route("", web::get().to(me))
                .service(web::scope("/friends").configure(friends::config)),
        )
        .route("/register", web::post().to(register))
        .route("/login", web::post().to(login));
    // .service(
    //     web::scope("/account")
    // .route("/register", web::post().to(register))
    // .route("/login", web::post().to(login)),
    // ;
}

async fn register(
    _req: HttpRequest,
    user_mgr: web::Data<Addr<user_manager::UserManager>>,
    payload: web::Form<user_manager::UserAuth>,
) -> HttpResponse {
    if let Ok(reg_res) = user_mgr
        .send(user_manager::msg::Register(payload.into_inner()))
        .await
    {
        match reg_res {
            Ok(_) => HR::Ok().json(ApiResponse::new("Registration successful.")),
            Err(api_err) => HR::Forbidden().json(ApiResponse::from_api_error(api_err)),
        }
    } else {
        HR::InternalServerError().json(ApiResponse::new("Registration failed. Internal Error."))
    }
}

async fn login(
    _req: HttpRequest,
    user_mgr: web::Data<Addr<user_manager::UserManager>>,
    payload: web::Form<user_manager::UserAuth>,
) -> HttpResponse {
    if let Ok(msg_res) = user_mgr
        .send(user_manager::msg::Login(payload.into_inner()))
        .await
    {
        if msg_res.is_ok() {
            HR::Ok().json(ApiResponse::new("Login successful."))
        } else {
            HR::Forbidden().json(ApiResponse::new("Login failed."))
        }
    } else {
        HR::InternalServerError().json(ApiResponse::new("Login failed. Internal Error."))
    }
}

#[allow(dead_code)]
async fn users(
    _: HttpRequest,
    user_mgr: web::Data<Addr<user_manager::UserManager>>,
) -> HttpResponse {
    let users_res: Result<Option<Vec<user::User>>, MailboxError> =
        user_mgr.send(user_manager::msg::GetUsers).await;
    if let Ok(Some(users)) = users_res {
        HttpResponse::Ok().json(users)
    } else {
        HttpResponse::InternalServerError().json(ApiResponse::new("Failed to retrieve users"))
    }
}

#[derive(Serialize, Deserialize)]
struct SearchQuery {
    search: String,
}

async fn search_user(
    _: HttpRequest,
    user_mgr: web::Data<Addr<user_manager::UserManager>>,
    query: web::Query<SearchQuery>,
) -> HR {
    let user_res: Result<Option<Vec<user::PublicUser>>, MailboxError> = user_mgr
        .send(user_manager::msg::SearchUsers(query.search.clone()))
        .await;
    if let Ok(Some(users)) = user_res {
        HR::Ok().json(users)
    } else {
        HR::InternalServerError().json(ApiResponse::new("Failed to retrieve users"))
    }
}

async fn me(
    _: HttpRequest,
    user_mgr: web::Data<Addr<user_manager::UserManager>>,
    payload: web::Form<user_manager::UserAuth>,
) -> HR {
    let user_res: Result<Option<user::PublicUser>, MailboxError> = user_mgr
        .send(user_manager::msg::GetUser(payload.into_inner()))
        .await;
    if let Ok(maybe_user) = user_res {
        if let Some(user) = maybe_user {
            HR::Ok().json(user)
        } else {
            HR::Forbidden().json(ApiResponse::new(
                "Could not find user. Invalid credentials.",
            ))
        }
    } else {
        HR::InternalServerError().json(ApiResponse::new("Failed to retrieve user"))
    }
}

mod friends {
    use super::*;
    use user::UserId;
    use user_manager::msg::*;

    pub fn config(cfg: &mut web::ServiceConfig) {
        cfg
            // .route("/", web::get().to(friends::get))
            .route("", web::post().to(friends::post))
            .route("/{id}", web::delete().to(friends::delete));
    }

    /*pub async fn get(
        user_mgr: web::Data<Addr<user_manager::UserManager>>,
        auth: web::Form<user_manager::UserAuth>,
    ) -> HR {
        let user_res: Result<bool, MailboxError> = user_mgr
            .send(UserAction {
                action: Action::FriendsAction(action),
                auth,
            })
            .await;
        if let Ok(b) = user_res {
            if b {
                HR::Ok().into()
            } else {
                HR::Forbidden().json(ApiResponse::new(
                    "Could not find user or invalid credentials.",
                ))
            }
        } else {
            HR::InternalServerError().json(ApiResponse::new("Failed to retrieve user"))
        }
    }*/

    pub async fn post(
        user_mgr: web::Data<Addr<user_manager::UserManager>>,
        auth: web::Form<user_manager::UserAuth>,
        query: web::Query<UserIdQuery>,
    ) -> HR {
        modify(
            FriendsAction::Add(query.id),
            user_mgr.get_ref(),
            auth.into_inner(),
        )
        .await
    }

    pub async fn delete(
        user_mgr: web::Data<Addr<user_manager::UserManager>>,
        auth: web::Form<user_manager::UserAuth>,
        id: web::Path<(UserId,)>,
    ) -> HR {
        modify(
            FriendsAction::Delete(id.0),
            user_mgr.get_ref(),
            auth.into_inner(),
        )
        .await
    }

    async fn modify(
        action: FriendsAction,
        user_mgr: &Addr<user_manager::UserManager>,
        auth: user_manager::UserAuth,
    ) -> HR {
        let user_res: Result<bool, MailboxError> = user_mgr
            .send(UserAction {
                action: Action::FriendsAction(action),
                auth,
            })
            .await;
        if let Ok(b) = user_res {
            if b {
                HR::Ok().into()
            } else {
                HR::Forbidden().json(ApiResponse::new(
                    "Could not find user or invalid credentials.",
                ))
            }
        } else {
            HR::InternalServerError().json(ApiResponse::new("Failed to retrieve user"))
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct UserIdQuery {
    id: user::UserId,
}

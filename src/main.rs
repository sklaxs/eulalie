mod auth;
mod bookextract;
mod templates;

use crate::bookextract::SourceKind;
use templates::*;
use anyhow::{Context, Result};
use askama::Template;
use axum::extract::{Multipart, Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;
use tower_sessions::Session;

const MAX_UPLOAD: usize = 64 * 1024 * 1024; // 64 Mo
const MAX_TEXT: usize = 2 * 1024 * 1024; // 2 Mo de texte par champ

pub struct AppState {
    pub db: SqlitePool,
    pub uploads_dir: PathBuf,
}

pub type SharedState = Arc<AppState>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .compact()
        .with_max_level(tracing::Level::INFO)
        .init();

    let db_path = std::env::var("EULALIE_DB").unwrap_or_else(|_| "eulalie.db".into());
    let uploads_dir =
        std::env::var("EULALIE_UPLOADS").unwrap_or_else(|_| "uploads".into());
    std::fs::create_dir_all(&uploads_dir)?;

    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .context("connexion SQLite")?;

    sqlx::migrate!("./migrations").run(&db).await?;

    let session_store = tower_sessions_sqlx_store::SqliteStore::new(db.clone());
    tower_sessions_sqlx_store::SqliteStore::migrate(&session_store)
        .await
        .context("migrations de sessions")?;

    let state: SharedState = Arc::new(AppState {
        db,
        uploads_dir: PathBuf::from(uploads_dir),
    });

    let session_layer = tower_sessions::SessionManagerLayer::new(session_store)
        // Cookies `Secure` uniquement si une URL publique HTTPS est déclarée.
// En local (HTTP sur 127.0.0.1), pas de `Secure` — sinon le navigateur
// refuse d'envoyer le cookie et la session ne persiste pas.
.with_secure(std::env::var("EULALIE_PUBLIC_URL").map(|u| u.starts_with("https://")).unwrap_or(false))
        .with_same_site(tower_sessions::cookie::SameSite::Strict)
        .with_http_only(true)
        .with_expiry(tower_sessions::Expiry::OnInactivity(time::Duration::hours(12)));

    let app = Router::new()
        .route("/", get(projects_page))
        .route("/register", get(register_page).post(register))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/projects", post(upload_project))
        .route("/projects/:id", get(project_home).post(rename_project))
        .route("/projects/:id/bookmark", post(add_bookmark))
        .route("/projects/:id/chapter/:idx", get(editor_page))
        .route("/projects/:id/chapter/:idx/save", post(save_chapter))
        .route("/projects/:id/chapter/:idx/preview", post(preview_chapter))
        .route("/projects/:id/export.md", get(export_markdown))
        .route("/healthz", get(healthz))
        // Limite de corps de requête alignée sur MAX_UPLOAD (Axum : 2 Mo par défaut,
        // trop bas pour un EPUB/PDF réel).
        .layer(axum::extract::DefaultBodyLimit::max(MAX_UPLOAD))
        .layer(session_layer)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("Eulalie écoute sur http://0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}

// ---------- auth ----------

async fn session_user(session: &Session) -> Option<(i64, String)> {
    session.get::<(i64, String)>("user").await.ok().flatten()
}

async fn require_user(
    session: &Session,
) -> Result<(i64, String), Redirect> {
    session_user(session).await.ok_or(Redirect::to("/login"))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn login_page(session: Session) -> impl IntoResponse {
    if session_user(&session).await.is_some() {
        return Redirect::to("/").into_response();
    }
    Html(LoginTemplate {
        error: None,
        mode_register: false,
    }
    .render()
    .unwrap())
    .into_response()
}

async fn register_page() -> Html<String> {
    Html(LoginTemplate {
        error: None,
        mode_register: true,
    }
    .render()
    .unwrap())
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

fn valid_username(u: &str) -> bool {
    let n = u.trim().len();
    (3..=32).contains(&n) && u.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
}

async fn register(
    State(state): State<SharedState>,
    session: Session,
    Form(form): Form<Credentials>,
) -> impl IntoResponse {
    let username = form.username.trim().to_string();
    if !valid_username(&username) {
        return Html(
            LoginTemplate {
                error: Some("Nom d'utilisateur : 3–32 caractères alphanumériques, - _ .".into()),
                mode_register: true,
            }
            .render()
            .unwrap(),
        )
        .into_response();
    }
    if form.password.len() < 10 {
        return Html(
            LoginTemplate {
                error: Some("Mot de passe : 10 caractères minimum.".into()),
                mode_register: true,
            }
            .render()
            .unwrap(),
        )
        .into_response();
    }
    let hash = match auth::hash_password(&form.password) {
        Ok(h) => h,
        Err(_) => return server_error(),
    };
    let res = sqlx::query("INSERT INTO users (username, password_hash) VALUES (?, ?)")
        .bind(&username)
        .bind(&hash)
        .execute(&state.db)
        .await;
    match res {
        Ok(_) => {
            let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
                .bind(&username)
                .fetch_one(&state.db)
                .await
                .unwrap_or(0);
            session.insert("user", (id, username)).await.unwrap();
            Redirect::to("/").into_response()
        }
        Err(sqlx::Error::Database(e)) if e.message().contains("UNIQUE") => Html(
            LoginTemplate {
                error: Some("Ce nom d'utilisateur est déjà pris.".into()),
                mode_register: true,
            }
            .render()
            .unwrap(),
        )
        .into_response(),
        Err(_) => server_error(),
    }
}

async fn login(
    State(state): State<SharedState>,
    session: Session,
    Form(form): Form<Credentials>,
) -> impl IntoResponse {
    let username = form.username.trim().to_string();
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, password_hash FROM users WHERE username = ?",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let ok = row
        .as_ref()
        .map(|(_, h)| auth::verify_password(&form.password, h))
        .unwrap_or(false);
    if ok {
        let (id, _) = row.unwrap();
        session.insert("user", (id, username)).await.unwrap();
        Redirect::to("/").into_response()
    } else {
        // Message générique : ne pas révéler si le compte existe (énumération).
        Html(
            LoginTemplate {
                error: Some("Identifiants invalides.".into()),
                mode_register: false,
            }
            .render()
            .unwrap(),
        )
        .into_response()
    }
}

async fn logout(session: Session) -> Redirect {
    let _ = session.flush().await;
    Redirect::to("/login")
}

fn server_error() -> axum::response::Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "erreur interne").into_response()
}

// ---------- projets ----------

async fn projects_page(
    State(state): State<SharedState>,
    session: Session,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let projects: Vec<ProjectRow> = sqlx::query_as(
        "SELECT p.id, p.title, p.source_kind, p.source_title, p.updated_at,
                (SELECT COUNT(*) FROM chapters WHERE project_id = p.id) AS chapter_count,
                (SELECT COUNT(*) FROM chapters WHERE project_id = p.id AND markdown != '') AS translated_count
         FROM projects p WHERE p.user_id = ? ORDER BY p.updated_at DESC",
    )
    .bind(user.0)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    Html(ProjectsTemplate { projects }.render().unwrap()).into_response()
}

/// Wrapper multipart : valide le type et la taille avant d'écrire sur disque.
async fn upload_project(
    State(state): State<SharedState>,
    session: Session,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let mut title: Option<String> = None;
    let mut file: Option<(String, Vec<u8>)> = None;
    while let Ok(Some(mut field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "title" => {
                if let Ok(t) = field.text().await {
                    title = Some(t.trim().chars().take(120).collect());
                }
            }
            "file" => {
                let filename = field
                    .file_name()
                    .unwrap_or("sans-nom")
                    .to_string();
                let kind = PathBuf::from(&filename)
                    .extension()
                    .and_then(|e| e.to_str())
                    .and_then(SourceKind::from_ext);
                let Some(kind) = kind else {
                    return (StatusCode::BAD_REQUEST, "seuls .epub et .pdf sont acceptés")
                        .into_response();
                };
                let mut data = Vec::new();
                // Toute erreur de lecture interrompt l'import : ne jamais
                // stocker un fichier partiellement reçu (corrompu).
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            data.extend_from_slice(&chunk);
                            if data.len() > MAX_UPLOAD {
                                return (StatusCode::PAYLOAD_TOO_LARGE, "fichier trop volumineux (max 64 Mo)")
                                    .into_response();
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::error!("lecture multipart interrompue : {e}");
                            return (StatusCode::BAD_REQUEST, "réception du fichier interrompue, réessayez")
                                .into_response();
                        }
                    }
                }
                if !bookextract::magic_matches(kind, &data) {
                    return (StatusCode::BAD_REQUEST, "contenu du fichier invalide")
                        .into_response();
                }
                file = Some((filename, data));
            }
            _ => {}
        }
    }
    let Some((filename, data)) = file else {
        return (StatusCode::BAD_REQUEST, "fichier manquant").into_response();
    };
    let Some((_, ext)) = filename.rsplit_once('.') else {
        return (StatusCode::BAD_REQUEST, "nom de fichier invalide").into_response();
    };
    let kind = SourceKind::from_ext(ext).unwrap();
    let project_title = title
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| filename.rsplit_once('.').map(|(b, _)| b.to_string()).unwrap_or(filename.clone()));

    // Persistance : fichier sur disque (nom aléatoire), métadonnées en base.
    let stored_name = format!("{}.{}", rand_name(), ext);
    let stored_path = state.uploads_dir.join(&stored_name);
    if let Err(e) = tokio::fs::write(&stored_path, &data).await {
        tracing::error!("écriture upload : {e}");
        return server_error();
    }

    // Extraction en thread bloquant (CPU-bound).
    let p = stored_path.clone();
    let extracted = match tokio::task::spawn_blocking(move || bookextract::extract(&p, kind)).await {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            let _ = tokio::fs::remove_file(&stored_path).await;
            tracing::error!("extraction : {e:#}");
            return (StatusCode::UNPROCESSABLE_ENTITY, "extraction impossible du document")
                .into_response();
        }
        Err(_) => return server_error(),
    };

    let mut tx = match state.db.begin().await {
        Ok(t) => t,
        Err(_) => return server_error(),
    };
    let res = async {
        let project_id: i64 = sqlx::query_scalar(
            "INSERT INTO projects (user_id, title, source_kind, source_path, source_title)
             VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(user.0)
        .bind(&project_title)
        .bind(kind.to_string())
        .bind(stored_path.to_string_lossy().to_string())
        .bind(&extracted.title)
        .fetch_one(&mut *tx)
        .await?;
        for (i, ch) in extracted.chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO chapters (project_id, idx, title, text) VALUES (?, ?, ?, ?)",
            )
            .bind(project_id)
            .bind(i as i64)
            .bind(&ch.title)
            .bind(&ch.text)
            .execute(&mut *tx)
            .await?;
        }
        Ok::<_, sqlx::Error>(project_id)
    }
    .await;
    match res {
        Ok(project_id) => {
            if tx.commit().await.is_err() {
                return server_error();
            }
            Redirect::to(&format!("/projects/{project_id}")).into_response()
        }
        Err(e) => {
            let _ = tx.rollback().await;
            let _ = tokio::fs::remove_file(&stored_path).await;
            tracing::error!("insertion projet : {e}");
            server_error()
        }
    }
}

fn rand_name() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------- éditeur ----------

async fn load_project(
    state: &SharedState,
    user_id: i64,
    project_id: i64,
) -> Result<ProjectMeta, Response> {
    let meta: Option<ProjectMeta> = sqlx::query_as(
        "SELECT id, title, source_kind, source_title FROM projects WHERE id = ? AND user_id = ?",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    meta.ok_or(StatusCode::NOT_FOUND.into_response())
}

use axum::response::Response;
use axum::Form;

async fn project_home(
    State(_state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
) -> impl IntoResponse {
    if require_user(&session).await.is_err() {
        return Redirect::to("/login").into_response();
    }
    Redirect::to(&format!("/projects/{project_id}/chapter/0")).into_response()
}

async fn editor_page(
    State(state): State<SharedState>,
    Path((project_id, idx)): Path<(i64, usize)>,
    session: Session,
) -> impl IntoResponse {
    editor_page_inner(state, project_id, idx, session).await
}

async fn editor_page_inner(
    state: SharedState,
    project_id: i64,
    idx: usize,
    session: Session,
) -> Response {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let Ok(project) = load_project(&state, user.0, project_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let chapters: Vec<ChapterMeta> = sqlx::query_as(
        "SELECT id, idx, title, markdown != '' AS has_translation FROM chapters
         WHERE project_id = ? ORDER BY idx",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let Some(current) = chapters.iter().find(|c| c.idx == idx as i64).cloned() else {
        return Redirect::to(&format!("/projects/{project_id}/chapter/0")).into_response();
    };
    let text_md: Option<String> = sqlx::query_scalar(
        "SELECT markdown FROM chapters WHERE project_id = ? AND idx = ?",
    )
    .bind(project_id)
    .bind(idx as i64)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let markdown = text_md.unwrap_or_default();
    let rendered = bookextract::markdown_to_html(&markdown);
    let source_text: Option<String> = sqlx::query_scalar(
        "SELECT text FROM chapters WHERE project_id = ? AND idx = ?",
    )
    .bind(project_id)
    .bind(idx as i64)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let current_lines: Vec<String> = source_text
        .unwrap_or_default()
        .split("\n\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let bookmarks: Vec<BookmarkMeta> = sqlx::query_as(
        "SELECT id, label, page FROM bookmarks WHERE project_id = ? ORDER BY id DESC",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let prev_idx = idx.checked_sub(1);
    let next_idx = if idx + 1 < chapters.len() { Some(idx + 1) } else { None };
    Html(
        EditorTemplate {
            project,
            chapters,
            bookmarks,
            current,
            current_idx: idx,
            current_lines,
            current_markdown: markdown,
            rendered_markdown: rendered,
            next_idx,
            prev_idx,
        }
        .render()
        .unwrap(),
    )
    .into_response()
}

#[derive(Deserialize)]
struct ChapterForm {
    markdown: String,
}

async fn save_chapter(
    State(state): State<SharedState>,
    Path((project_id, idx)): Path<(i64, usize)>,
    session: Session,
    Form(form): Form<ChapterForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if form.markdown.len() > MAX_TEXT {
        return (StatusCode::PAYLOAD_TOO_LARGE, "texte trop long").into_response();
    }
    let res = sqlx::query(
        "UPDATE chapters SET markdown = ? WHERE project_id = ? AND idx = ?",
    )
    .bind(&form.markdown)
    .bind(project_id)
    .bind(idx as i64)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => {
            let _ = sqlx::query("UPDATE projects SET updated_at = datetime('now') WHERE id = ?")
                .bind(project_id)
                .execute(&state.db)
                .await;
            Redirect::to(&format!("/projects/{project_id}/chapter/{idx}"))
                .into_response()
        }
        Err(_) => server_error(),
    }
}

async fn preview_chapter(
    State(state): State<SharedState>,
    Path((project_id, _idx)): Path<(i64, usize)>,
    session: Session,
    Form(form): Form<ChapterForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if form.markdown.len() > MAX_TEXT {
        return (StatusCode::PAYLOAD_TOO_LARGE, "texte trop long").into_response();
    }
    let html = bookextract::markdown_to_html(&form.markdown);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

#[derive(Deserialize)]
struct RenameForm {
    title: String,
}

async fn rename_project(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
    Form(form): Form<RenameForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let title = form.title.trim().chars().take(120).collect::<String>();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "titre vide").into_response();
    }
    let _ = sqlx::query("UPDATE projects SET title = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(&title)
        .bind(project_id)
        .execute(&state.db)
        .await;
    Redirect::to(&format!("/projects/{project_id}")).into_response()
}

#[derive(Deserialize)]
struct BookmarkForm {
    label: String,
}

async fn add_bookmark(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
    Form(form): Form<BookmarkForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let label = form.label.trim().chars().take(80).collect::<String>();
    let page = session
        .get::<usize>("current_idx")
        .await
        .ok()
        .flatten()
        .unwrap_or(0);
    let _ = sqlx::query("INSERT INTO bookmarks (project_id, label, page) VALUES (?, ?, ?)")
        .bind(project_id)
        .bind(&label)
        .bind(page as i64)
        .execute(&state.db)
        .await;
    Redirect::to(&format!("/projects/{project_id}")).into_response()
}

async fn export_markdown(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT title, markdown FROM chapters WHERE project_id = ? AND markdown != '' ORDER BY idx",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let title: String = sqlx::query_scalar("SELECT source_title FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or_else(|_| "Traduction".into());
    let mut md = format!("# {title}\n\n");
    for (ch_title, text) in rows {
        md.push_str(&format!("## {ch_title}\n\n{text}\n\n"));
    }
    (
        [
            (header::CONTENT_TYPE, "text/markdown; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"export.md\"",
            ),
        ],
        md,
    )
        .into_response()
}

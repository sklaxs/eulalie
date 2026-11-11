mod auth;
mod bookextract;
mod templates;

use templates::{LangOption, UserSettings};

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
        .route("/projects/:id/images", post(upload_image))
        .route("/images/:image_id", get(serve_image))
        .route("/images/:image_id/rename", post(rename_image))
        .route("/images/:image_id/delete", post(delete_image))
        .route("/projects/:id/langs", post(set_project_langs))
        .route("/projects/:id/glossary", post(add_glossary_term))
        .route("/glossary/:id/delete", post(delete_glossary_term))
        .route("/projects/:id/glossary.csv", get(export_glossary_csv))
        .route("/projects/:id/glossary.json", get(glossary_json))
        .route("/projects/:id/glossary/import", post(import_glossary))
        .route("/projects/:id/notes", post(save_notes))
        .route("/settings", get(settings_page).post(save_settings))
        .route("/translate", post(translate_text))
        .route("/healthz", get(healthz))
        .route("/favicon.svg", get(favicon_svg))
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

/// Favicon (SVG Tabler, licence MIT) — servi inline pour éviter toute
/// dépendance externe ; pas de lecture disque (conteneur read-only).
async fn favicon_svg() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml"),
         (header::CACHE_CONTROL, "public, max-age=3600"),],
        include_str!("../static/favicon.svg"),
    )
}

async fn login_page(session: Session) -> impl IntoResponse {
    if session_user(&session).await.is_some() {
        return Redirect::to("/").into_response();
    }
    let allow_register = !registration_disabled();
    Html(LoginTemplate {
        error: None,
        mode_register: false,
        allow_register,
    }
    .render()
    .unwrap())
    .into_response()
}

fn registration_disabled() -> bool {
    std::env::var("EULALIE_DISABLE_REGISTRATION").is_ok()
}

async fn register_page() -> impl IntoResponse {
    if registration_disabled() {
        return StatusCode::NOT_FOUND.into_response();
    }
    Html(LoginTemplate {
        error: None,
        mode_register: true,
        allow_register: true,
    }
    .render()
    .unwrap())
    .into_response()
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
    if registration_disabled() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let username = form.username.trim().to_string();
    if !valid_username(&username) {
        return Html(
            LoginTemplate {
                error: Some("Nom d'utilisateur : 3–32 caractères alphanumériques, - _ .".into()),
                mode_register: true,
                allow_register: true,
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
                allow_register: true,
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
                allow_register: true,
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
                allow_register: !registration_disabled(),
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
) -> Result<ProjectMeta, Box<Response>> {
    let meta: Option<ProjectMeta> = sqlx::query_as(
        "SELECT id, title, source_kind, source_title, source_lang, target_lang FROM projects WHERE id = ? AND user_id = ?",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    meta.ok_or_else(|| Box::new(StatusCode::NOT_FOUND.into_response()))
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
    let settings = load_settings(&state.db, user.0).await;
    let images: Vec<ImageRow> = sqlx::query_as(
        "SELECT id, filename FROM project_images WHERE project_id = ? ORDER BY id DESC",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let glossary: Vec<GlossaryRow> = sqlx::query_as(
        "SELECT id, term, translation, mistranslation FROM glossary WHERE project_id = ? ORDER BY term",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let notes: String = sqlx::query_scalar("SELECT notes FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or_default();
    let all_langs = ["en", "fr", "de", "es", "it", "pt", "nl", "ru", "zh", "ja", "ar"];
    let langs: Vec<LangOption> = all_langs
        .iter()
        .map(|code| LangOption {
            code: code.to_string(),
            selected_src: *code == project.source_lang,
            selected_tgt: *code == project.target_lang,
        })
        .collect();
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
            autosave: settings.autosave,
            translate_server: settings.translate_server,
            translate_timeout: settings.translate_timeout,
            shortcut_prev: settings.shortcut_prev,
            shortcut_next: settings.shortcut_next,
            shortcut_preview: settings.shortcut_preview,
            shortcut_glossary: settings.shortcut_glossary,
            shortcut_notes: settings.shortcut_notes,
            shortcut_translate: settings.shortcut_translate,
            notes_side: settings.notes_side,
            notes_close_outside: settings.notes_close_outside,
            images,
            langs,
            glossary,
            notes,
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
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT markdown FROM chapters WHERE project_id = ? AND markdown != '' ORDER BY idx",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    // Export du contenu uniquement — sans titre du document ni titres de
    // chapitres (les titres appartiennent au texte rédigé par l'utilisateur).
    let mut md = String::new();
    for text in rows {
        md.push_str(&text);
        md.push_str("\n\n---\n\n");
    }
    md = md.trim_end_matches("---\n\n").trim_end().to_string();
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

// ---------- images de projet ----------

const ALLOWED_IMAGE_MIME: &[(&str, &[u8])] = &[
    ("image/png", &[0x89, b'P', b'N', b'G']),
    ("image/jpeg", &[0xFF, 0xD8, 0xFF]),
    ("image/gif", b"GIF8"),
];

async fn upload_image(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(mut field) = multipart.next_field().await.ok().flatten() else {
        return (StatusCode::BAD_REQUEST, "fichier manquant").into_response();
    };
    let filename = field.file_name().unwrap_or("image").chars().take(120).collect::<String>();
    let mut data = Vec::new();
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                data.extend_from_slice(&chunk);
                if data.len() > MAX_UPLOAD {
                    return (StatusCode::PAYLOAD_TOO_LARGE, "image trop volumineuse").into_response();
                }
            }
            Ok(None) => break,
            Err(_) => return (StatusCode::BAD_REQUEST, "réception interrompue").into_response(),
        }
    }
    let Some((mime, _)) = ALLOWED_IMAGE_MIME.iter().find(|(_, magic)| data.starts_with(magic)) else {
        return (StatusCode::BAD_REQUEST, "seuls PNG, JPEG et GIF sont acceptés").into_response();
    };
    let stored_name = format!("{}.{}", rand_name(), mime.strip_prefix("image/").unwrap_or("bin"));
    let stored_path = state.uploads_dir.join(&stored_name);
    if tokio::fs::write(&stored_path, &data).await.is_err() {
        return server_error();
    }
    let res = sqlx::query(
        "INSERT INTO project_images (project_id, filename, stored_path, mime) VALUES (?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(&filename)
    .bind(stored_path.to_string_lossy().to_string())
    .bind(*mime)
    .execute(&state.db)
    .await;
    match res {
        Ok(done) => {
            let id = done.last_insert_rowid();
            (
                [(header::CONTENT_TYPE, "application/json")],
                format!("{{\"id\":{id},\"filename\":\"{filename}\"}}"),
            )
                .into_response()
        }
        Err(_) => server_error(),
    }
}

#[derive(Deserialize)]
struct RenameImageForm {
    filename: String,
}

async fn rename_image(
    State(state): State<SharedState>,
    Path(image_id): Path<i64>,
    session: Session,
    Form(form): Form<RenameImageForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    // N'affiche jamais les chemins : le nom devient juste une étiquette Markdown propre.
    let name: String = form
        .filename
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        .take(120)
        .collect();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "nom invalide").into_response();
    }
    let res = sqlx::query(
        "UPDATE project_images SET filename = ? WHERE id = ? AND project_id IN (SELECT id FROM projects WHERE user_id = ?)",
    )
    .bind(&name)
    .bind(image_id)
    .bind(user.0)
    .execute(&state.db)
    .await;
    match res {
        Ok(done) if done.rows_affected() == 1 => {
            ([(header::CONTENT_TYPE, "application/json")], format!("{{\"filename\":\"{name}\"}}")).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct LangsForm {
    source_lang: String,
    target_lang: String,
}

/// Langues acceptées (sous-ensemble des modèles Argos courants).
const LANGS: &[&str] = &["en", "fr", "de", "es", "it", "pt", "nl", "ru", "zh", "ja", "ar"];

fn valid_lang(s: &str) -> Option<&'static str> {
    LANGS.iter().find(|l| **l == s).copied()
}

async fn set_project_langs(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
    Form(form): Form<LangsForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (Some(src), Some(tgt)) = (valid_lang(&form.source_lang), valid_lang(&form.target_lang))
    else {
        return (StatusCode::BAD_REQUEST, "langue non prise en charge").into_response();
    };
    if src == tgt {
        return (StatusCode::BAD_REQUEST, "langues identiques").into_response();
    }
    let _ = sqlx::query("UPDATE projects SET source_lang = ?, target_lang = ? WHERE id = ?")
        .bind(src)
        .bind(tgt)
        .bind(project_id)
        .execute(&state.db)
        .await;
    Redirect::to(&format!("/projects/{project_id}/chapter/0")).into_response()
}

// ---------- glossaire par projet ----------

#[derive(Deserialize)]
struct GlossaryForm {
    term: String,
    translation: String,
    mistranslation: Option<String>,
}

const MAX_TERM: usize = 200;
const MAX_TRANSLATION: usize = 2000;

async fn add_glossary_term(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
    Form(form): Form<GlossaryForm>,
) -> Response {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let term = form.term.trim().to_string();
    let translation = form.translation.trim().to_string();
    // Mistraduction : la traduction erronée que LibreTranslate produit
    // habituellement pour ce terme — utile pour retrouver l'entrée sans
    // repartir du mot dans la langue d'origine.
    let mistranslation = form
        .mistranslation
        .as_deref()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && m.len() <= MAX_TRANSLATION);
    if term.is_empty() || translation.is_empty() || term.len() > MAX_TERM || translation.len() > MAX_TRANSLATION {
        return (StatusCode::BAD_REQUEST, "terme ou traduction invalide").into_response();
    }
    // Upsert insensible à la casse (COLLATE NOCASE) : « Accountability » et
    // « accountability » désignent le même terme — on met à jour la ligne
    // existante plutôt que d'en créer une seconde.
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM glossary WHERE project_id = ? AND term = ? COLLATE NOCASE",
    )
    .bind(project_id)
    .bind(&term)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let res = if let Some(id) = existing {
        sqlx::query("UPDATE glossary SET translation = ?, mistranslation = ? WHERE id = ?")
            .bind(&translation)
            .bind(&mistranslation)
            .bind(id)
            .execute(&state.db)
            .await
            .map(|_| id)
    } else {
        sqlx::query("INSERT INTO glossary (project_id, term, translation, mistranslation) VALUES (?, ?, ?, ?)")
            .bind(project_id)
            .bind(&term)
            .bind(&translation)
            .bind(&mistranslation)
            .execute(&state.db)
            .await
            .map(|r| r.last_insert_rowid())
    };
    match res {
        Ok(id) => {
            axum::Json(serde_json::json!({"id": id, "term": term, "translation": translation, "mistranslation": mistranslation})).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "insertion impossible").into_response(),
    }
}

async fn delete_glossary_term(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
    session: Session,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    // Le glossaire appartient à un projet : la propriété est revalidée.
    let res = sqlx::query(
        "DELETE FROM glossary WHERE id = ? AND project_id IN (SELECT id FROM projects WHERE id = glossary.project_id AND user_id = ?)",
    )
    .bind(id)
    .bind(user.0)
    .execute(&state.db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => StatusCode::OK.into_response(),
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "suppression impossible").into_response(),
    }
}

// ---------- notes de traduction par projet (prise de notes libre) ----------

#[derive(Deserialize)]
struct NotesForm {
    notes: String,
}

async fn save_notes(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
    Form(form): Form<NotesForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if form.notes.len() > MAX_TEXT {
        return (StatusCode::BAD_REQUEST, "notes trop longues").into_response();
    }
    let res = sqlx::query("UPDATE projects SET notes = ? WHERE id = ?")
        .bind(&form.notes)
        .bind(project_id)
        .execute(&state.db)
        .await;
    if res.is_ok() {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "enregistrement impossible").into_response()
    }
}

// ---------- export du glossaire (CSV) ----------

/// Champ CSV entre guillemets, guillemets doublés (RFC 4180).
fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Liste JSON du glossaire (rafraîchissement du volet après import).
async fn glossary_json(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
) -> Response {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let entries: Vec<GlossaryRow> = sqlx::query_as(
        "SELECT id, term, translation, mistranslation FROM glossary WHERE project_id = ? ORDER BY term",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let rows: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|g| {
            serde_json::json!({
                "id": g.id,
                "term": g.term,
                "translation": g.translation,
                "mistranslation": g.mistranslation,
            })
        })
        .collect();
    axum::Json(rows).into_response()
}

/// Parseur CSV minimal (RFC 4180) : champs séparés par `;`, guillemets
/// doublés pour échapper, retours à la ligne CRLF tolérés.
fn parse_csv(content: &str) -> Vec<Vec<String>> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = content.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ';' if !in_quotes => {
                row.push(std::mem::take(&mut field));
            }
            '\n' if !in_quotes => {
                if field.ends_with('\r') {
                    field.pop();
                }
                row.push(std::mem::take(&mut field));
                if !row.iter().all(|f| f.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if !row.iter().all(|f| f.trim().is_empty()) {
            rows.push(row);
        }
    }
    rows
}

/// Import d'un glossaire CSV (mêmes colonnes que l'export : terme;traduction;mistraduction).
async fn import_glossary(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
    mut multipart: Multipart,
) -> Response {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut content: Option<String> = None;
    while let Some(field) = multipart.next_field().await.ok().flatten() {
        if field.name() == Some("file") {
            let data = field.bytes().await.unwrap_or_default();
            if data.len() > MAX_TEXT {
                return (StatusCode::BAD_REQUEST, "fichier trop volumineux").into_response();
            }
            content = Some(String::from_utf8_lossy(&data).into_owned());
        }
    }
    let Some(content) = content else {
        return (StatusCode::BAD_REQUEST, "aucun fichier reçu").into_response();
    };
    let mut rows = parse_csv(&content);
    // Ligne d'en-tête optionnelle (comme dans l'export).
    if rows
        .first()
        .and_then(|r| r.first())
        .map(|t| t.trim().to_lowercase() == "terme")
        .unwrap_or(false)
    {
        rows.remove(0);
    }
    let mut imported = 0usize;
    for row in rows {
        if row.len() < 2 {
            continue;
        }
        let term = row[0].trim().to_string();
        let translation = row[1].trim().to_string();
        let mistranslation = row.get(2).map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
        if term.is_empty() || translation.is_empty() || term.len() > MAX_TERM || translation.len() > MAX_TRANSLATION {
            continue;
        }
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM glossary WHERE project_id = ? AND term = ? COLLATE NOCASE",
        )
        .bind(project_id)
        .bind(&term)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);
        let ok = if let Some(id) = existing {
            sqlx::query("UPDATE glossary SET translation = ?, mistranslation = ? WHERE id = ?")
                .bind(&translation)
                .bind(&mistranslation)
                .bind(id)
                .execute(&state.db)
                .await
                .is_ok()
        } else {
            sqlx::query("INSERT INTO glossary (project_id, term, translation, mistranslation) VALUES (?, ?, ?, ?)")
                .bind(project_id)
                .bind(&term)
                .bind(&translation)
                .bind(&mistranslation)
                .execute(&state.db)
                .await
                .is_ok()
        };
        if ok {
            imported += 1;
        }
    }
    axum::Json(serde_json::json!({"imported": imported})).into_response()
}

async fn export_glossary_csv(
    State(state): State<SharedState>,
    Path(project_id): Path<i64>,
    session: Session,
) -> Response {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if load_project(&state, user.0, project_id).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let entries: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT term, translation, mistranslation FROM glossary WHERE project_id = ? ORDER BY term",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    // BOM UTF-8 pour qu'Excel ouvre correctement les accents.
    let mut csv = String::from("\u{feff}terme;traduction;mistraduction\r\n");
    for (term, tr, mis) in entries {
        csv.push_str(&csv_field(&term));
        csv.push(';');
        csv.push_str(&csv_field(&tr));
        csv.push(';');
        csv.push_str(&csv_field(mis.as_deref().unwrap_or("")));
        csv.push_str("\r\n");
    }
    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"glossaire.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}

async fn delete_image(
    State(state): State<SharedState>,
    Path(image_id): Path<i64>,
    session: Session,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let row: Option<String> = sqlx::query_scalar(
        "SELECT i.stored_path FROM project_images i
         JOIN projects p ON p.id = i.project_id
         WHERE i.id = ? AND p.user_id = ?",
    )
    .bind(image_id)
    .bind(user.0)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let Some(path) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let res = sqlx::query("DELETE FROM project_images WHERE id = ?")
        .bind(image_id)
        .execute(&state.db)
        .await;
    if res.is_ok() {
        let _ = tokio::fs::remove_file(&path).await;
        ([(header::CONTENT_TYPE, "application/json")], "{}").into_response()
    } else {
        server_error()
    }
}

async fn serve_image(
    State(state): State<SharedState>,
    Path(image_id): Path<i64>,
    session: Session,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT i.stored_path, i.mime FROM project_images i
         JOIN projects p ON p.id = i.project_id
         WHERE i.id = ? AND p.user_id = ?",
    )
    .bind(image_id)
    .bind(user.0)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let Some((path, mime)) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "private, max-age=86400".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------- paramètres ----------

#[derive(Deserialize, Default)]
struct SettingsForm {
    autosave: Option<String>,
    shortcut_prev: Option<String>,
    shortcut_next: Option<String>,
    shortcut_preview: Option<String>,
    translate_server: Option<String>,
    translate_timeout: Option<String>,
    shortcut_glossary: Option<String>,
    shortcut_notes: Option<String>,
    shortcut_translate: Option<String>,
    notes_side: Option<String>,
    notes_close_outside: Option<String>,
}

async fn load_settings(db: &SqlitePool, user_id: i64) -> UserSettings {
    let row: Option<(i64, String, Option<String>, i64, String, i64)> = sqlx::query_as(
        "SELECT s.autosave, s.shortcuts, s.translate_server, s.translate_timeout, s.notes_side, s.notes_close_outside FROM user_settings s WHERE s.user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);
    match row {
        Some((autosave, shortcuts, server, timeout, side, close_outside)) => {
            let mut s = UserSettings {
                autosave: autosave != 0,
                shortcut_prev: "Alt+ArrowLeft".into(),
                shortcut_next: "Alt+ArrowRight".into(),
                shortcut_preview: "ALT+ArrowUp".into(),
                shortcut_glossary: "Alt+G".into(),
                shortcut_notes: "Alt+N".into(),
                shortcut_translate: "Alt+T".into(),
                translate_server: server,
                translate_timeout: timeout.clamp(1, 120),
                notes_side: if side == "left" { "left".into() } else { "right".into() },
                notes_close_outside: close_outside != 0,
            };
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&shortcuts) {
                if let Some(p) = v.get("prev").and_then(|x| x.as_str()) {
                    s.shortcut_prev = p.to_string();
                }
                if let Some(n) = v.get("next").and_then(|x| x.as_str()) {
                    s.shortcut_next = n.to_string();
                }
                if let Some(pr) = v.get("preview").and_then(|x| x.as_str()) {
                    s.shortcut_preview = pr.to_string();
                }
                if let Some(g) = v.get("glossary").and_then(|x| x.as_str()) {
                    s.shortcut_glossary = g.to_string();
                }
                if let Some(n) = v.get("notes").and_then(|x| x.as_str()) {
                    s.shortcut_notes = n.to_string();
                }
                if let Some(t) = v.get("translate").and_then(|x| x.as_str()) {
                    s.shortcut_translate = t.to_string();
                }
            }
            s
        }
        None => UserSettings {
            autosave: true,
            shortcut_prev: "Alt+ArrowLeft".into(),
            shortcut_next: "Alt+ArrowRight".into(),
            shortcut_preview: "ALT+ArrowUp".into(),
            // Préréglage : le service libretranslate du docker-compose (interne).
            translate_server: Some("http://libretranslate:5000".into()),
            translate_timeout: 10,
            shortcut_glossary: "Alt+G".into(),
            shortcut_notes: "Alt+N".into(),
            shortcut_translate: "Alt+T".into(),
            notes_side: "right".into(),
            notes_close_outside: true,
        },
    }
}

async fn settings_page(
    State(state): State<SharedState>,
    session: Session,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let s = load_settings(&state.db, user.0).await;
    let images_count: i64 = 0;
    let tpl = crate::templates::SettingsTemplate {
        settings: s,
        saved: false,
        images_count,
    };
    Html(tpl.render().unwrap()).into_response()
}

async fn save_settings(
    State(state): State<SharedState>,
    session: Session,
    Form(form): Form<SettingsForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let autosave = form.autosave.as_deref() == Some("on") || form.autosave.as_deref() == Some("true");
    let shortcuts = serde_json::json!({
        "prev": sanitize_shortcut(form.shortcut_prev.as_deref().unwrap_or("Alt+ArrowLeft")),
        "next": sanitize_shortcut(form.shortcut_next.as_deref().unwrap_or("Alt+ArrowRight")),
        "preview": sanitize_shortcut(form.shortcut_preview.as_deref().unwrap_or("ALT+ArrowUp")),
        "glossary": sanitize_shortcut(form.shortcut_glossary.as_deref().unwrap_or("Alt+G")),
        "notes": sanitize_shortcut(form.shortcut_notes.as_deref().unwrap_or("Alt+N")),
        "translate": sanitize_shortcut(form.shortcut_translate.as_deref().unwrap_or("Alt+T")),
    })
    .to_string();
    // Côté d'affichage du volet Notes : gauche ou droite (droite par défaut).
    let notes_side = match form.notes_side.as_deref() {
        Some("left") => "left",
        _ => "right",
    };
    // Fermer le volet Notes quand on clique à l'extérieur (activé par défaut).
    let notes_close_outside = form.notes_close_outside.as_deref() == Some("on")
        || form.notes_close_outside.as_deref() == Some("true");
    // Accepte tout serveur http:// ou https:// ; le slash final est
    // retiré pour que "{server}/translate" reste une URL valide.
    let server = form
        .translate_server
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"));
    // Temps d'attente maximal des traductions (1 à 120 secondes, 10 s par défaut).
    let timeout = form
        .translate_timeout
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(10)
        .clamp(1, 120);
    let res = sqlx::query(
        "INSERT INTO user_settings (user_id, autosave, shortcuts, translate_server, translate_timeout, notes_side, notes_close_outside) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(user_id) DO UPDATE SET autosave = excluded.autosave, shortcuts = excluded.shortcuts, translate_server = excluded.translate_server, translate_timeout = excluded.translate_timeout, notes_side = excluded.notes_side, notes_close_outside = excluded.notes_close_outside",
    )
    .bind(user.0)
    .bind(autosave as i64)
    .bind(&shortcuts)
    .bind(&server)
    .bind(timeout)
    .bind(notes_side)
    .bind(notes_close_outside as i64)
    .execute(&state.db)
    .await;
    let s = load_settings(&state.db, user.0).await;
    let tpl = crate::templates::SettingsTemplate {
        settings: s,
        saved: res.is_ok(),
        images_count: 0,
    };
    Html(tpl.render().unwrap()).into_response()
}

/// Ne conserve que des combinaisons sûres : Alt/Ctrl/Shift/Meta + touche.
fn sanitize_shortcut(s: &str) -> String {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let mut mods = Vec::new();
    let mut key = String::new();
    for p in parts {
        match p.to_ascii_lowercase().as_str() {
            "alt" | "ctrl" | "control" | "shift" | "meta" => mods.push(p.to_ascii_uppercase()),
            _ => {
                if key.is_empty() && p.len() <= 20 && p.chars().all(|c| c.is_alphanumeric()) {
                    key = p.to_string();
                }
            }
        }
    }
    if key.is_empty() {
        "Alt+ArrowLeft".into()
    } else {
        let mut out = mods.join("+");
        if !out.is_empty() {
            out.push('+');
        }
        out.push_str(&key);
        out
    }
}

// ---------- traduction (LibreTranslate) ----------

#[derive(Deserialize)]
struct TranslateForm {
    text: String,
    project_id: Option<i64>,
}

async fn translate_text(
    State(state): State<SharedState>,
    session: Session,
    Form(form): Form<TranslateForm>,
) -> impl IntoResponse {
    let Ok(user) = require_user(&session).await else {
        return Redirect::to("/login").into_response();
    };
    if form.text.trim().is_empty() || form.text.len() > MAX_TEXT {
        return (StatusCode::BAD_REQUEST, "texte invalide").into_response();
    }
    let (server, timeout) = sqlx::query_as::<_, (Option<String>, i64)>(
        "SELECT translate_server, translate_timeout FROM user_settings WHERE user_id = ?",
    )
    .bind(user.0)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|(s, t)| (s, t.clamp(1, 120)))
    .unwrap_or_else(|| (Some("http://libretranslate:5000".into()), 10));
    let server = server.unwrap_or_else(|| "http://libretranslate:5000".into());
    // Langues du projet si précisé (et appartenant à l'utilisateur), sinon en→fr.
    let (mut source, mut target) = ("en".to_string(), "fr".to_string());
    if let Some(pid) = form.project_id {
        let langs: Option<(String, String)> = sqlx::query_as(
            "SELECT source_lang, target_lang FROM projects WHERE id = ? AND user_id = ?",
        )
        .bind(pid)
        .bind(user.0)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);
        if let Some((s, tg)) = langs {
            source = s;
            target = tg;
        }
    }
    let body = serde_json::json!({
        "q": form.text,
        "source": source,
        "target": target,
        "format": "text",
    });
    let res = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let r = ureq::post(&format!("{server}/translate"))
            .timeout(std::time::Duration::from_secs(timeout as u64))
            .send_json(body)
            .map_err(|e| e.to_string())?;
        r.into_json::<serde_json::Value>().map_err(|e| e.to_string())
    })
    .await;
    match res {
        Ok(Ok(v)) if v.get("translatedText").is_some() => {
            let translated = v
                .get("translatedText")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            if translated.is_empty() {
                (StatusCode::BAD_GATEWAY, "réponse du serveur de traduction invalide").into_response()
            } else {
                ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], translated).into_response()
            }
        }
        _ => (StatusCode::BAD_GATEWAY, "serveur de traduction injoignable").into_response(),
    }
}

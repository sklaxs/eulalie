# Eulalie

**Eulalie** est la déclinaison web d'[Ophélie](https://github.com/sklaxs/ophelie) :
un atelier de traduction EPUB/PDF vers Markdown, entièrement en Rust.

## Architecture

- **Backend** : Axum (Rust), templates Askama (côté serveur, sans JS framework),
  SQLite via SQLx (migrations incluses).
- **Authentification** : Argon2id (crate `argon2`, implémentation de référence),
  sessions serveur via `tower-sessions` (store SQLx, cookie `HttpOnly`,
  `SameSite=Strict`, `Secure` en production).
- **Conteneur** : build multi-stage (Rust slim → Alpine), utilisateur non-root,
  `read_only`, `cap_drop: ALL`, `no-new-privileges`, healthcheck.

```mermaid
browser ──HTTP──> eulalie (Axum, :8080)
                    ├── SQLite (sessions, projets, chapitres, favoris)
                    └── /data/uploads (fichiers EPUB/PDF persistés)
```

Un seul conteneur suffit : la base SQLite est embarquée (pas de SGBD externe),
les fichiers importés sont stockés sur un volume Docker.

## Fonctionnalités

- Multi-utilisateur, multi-projets (titre par défaut = nom du fichier, modifiable).
- Import EPUB/PDF persisté en base : pas de réimport à chaque session.
- Éditeur deux panneaux : source à gauche, traduction Markdown à droite
  (barre d'outils gras/italique/titres/tableau/image/lien, autosauvegarde 1,2 s,
  aperçu, zoom Ctrl+molette par panneau, navigation Alt+flèches).
- Export Markdown d'un projet complet.
- Points de repère (bookmarks) par projet.

## Démarrage

```bash
docker compose up -d --build
# puis ouvrir http://localhost:8080
```

Variables d'environnement :

| Variable | Défaut | Rôle |
|---|---|---|
| `EULALIE_DB` | `/data/db/eulalie.db` | chemin SQLite |
| `EULALIE_UPLOADS` | `/data/uploads` | répertoire des fichiers importés |
| `EULALIE_INSECURE_COOKIES` | (absent) | à définir uniquement pour un test en HTTP local (désactive `Secure`) |
| `RUST_LOG` | `eulalie=info` | verbosité des logs |

### Production

En production, exposez derrière un reverse proxy TLS (Caddy, nginx, Traefik) :
le port n'est publié que sur `127.0.0.1` par défaut dans le compose. Les cookies
sont alors `Secure` automatiquement.

## Sécurité (référentiel OWASP)

Mesures implémentées, point par point :

### A01 Broken Access Control
- Chaque requête sur un projet/chapitre vérifie `project.user_id = session.user_id`
  (testé : un utilisateur B accédant au projet de A reçoit 404).
- Anonyme → redirection `/login` (pas de fuite de contenu).

### A02 Cryptographic Failures
- Mots de passe hachés en **Argon2id** (recommandation OWASP Password Storage).
- Cookies de session `HttpOnly` + `SameSite=Strict` + `Secure` (sauf dev).

### A03 Injection (SQLi, XSS)
- **SQLi** : 100 % de requêtes préparées SQLx (bind de paramètres), aucune
  concaténation de chaîne SQL.
- **XSS** : le Markdown rendu est assaini par **ammonia** (liste blanche) —
  testé : `<script>alert(1)</script>` est neutralisé. Les templates Askama
  échappent par défaut toutes les variables.
- Pas d'exécution de commandes, pas d'`eval` côté client.

### A04 Insecure Design
- Un document appartient à un utilisateur ; les IDs sont des scalaires, la
  propriété est revalidée à chaque handler (pas de "confiance au client").

### A05 Security Misconfiguration
- Conteneur : utilisateur dédié non-root, filesystem `read_only`, tmpfs `/tmp`,
  `cap_drop: ALL`, `no-new-privileges`, image Alpine minimale.
- Pas de débogage en production, logs compacts.

### A06 Vulnerable Components
- Dépendances 100 % Rust, versions épinglées dans `Cargo.lock`. Vérifiez avec
  `cargo audit`.

### A07 Identification & Authentication Failures
- Argon2id (anti brute-force par coût mémoire), politique : identifiant 3–32
  caractères, mot de passe ≥ 10 caractères.
- Message d'erreur générique (« Identifiants invalides ») pour ne pas révéler
  l'existence d'un compte.
- Sessions expirant après 12 h d'inactivité.

### A08 Software & Data Integrity
- Migrations SQL versionnées et jouées au démarrage (sqlx::migrate).
- Magic bytes vérifiés à l'import (un `.epub` doit être une archive ZIP,
  un `.pdf` doit commencer par `%PDF`) : un fichier renommé est rejeté.

### A09 Logging & Monitoring
- Journalisation structurée (`tracing`) des erreurs serveur et du démarrage.

### A10 SSRF
- Aucune requête sortante initiée par le serveur (pas de fetch d'URL fournie
  par l'utilisateur, pas de preview d'URL distante).

### Uploads (A04/A08)
- Taille limitée (64 Mo), extension **et** magic bytes contrôlés,
  nom de stockage aléatoire (pas d'écrasement ni de path traversal),
  limite de taille sur tous les champs texte (2 Mo).

## Développement

```bash
cargo run   # écoute sur :8080, crée eulalie.db
cargo test  # tests unitaires (extraction, auth, assainissement Markdown)
```

## Limites

- PDF : extraction texte uniquement (pas de rendu page).
- Pas encore de rate limiting sur /login (à ajouter avec un reverse proxy,
  p. ex. `limit_req` nginx ou fail2ban).

## Licence

MIT

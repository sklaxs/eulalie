# Eulalie

**Eulalie** est la déclinaison web d'[Ophélie](https://github.com/sklaxs/ophelie) :
un atelier de traduction EPUB/PDF vers Markdown, entièrement en Rust.

## Architecture

- **Backend** : Axum (Rust), templates Askama (côté serveur, sans JS framework),
  SQLite via SQLx (migrations incluses).
- **Authentification** : Argon2id (crate `argon2`, implémentation de référence),
  sessions serveur via `tower-sessions` (store SQLx, cookie `HttpOnly`,
  `SameSite=Strict`, `Secure` activé via `EULALIE_PUBLIC_URL` en production).
- **Conteneur** : build multi-stage (Rust slim → Alpine), utilisateur non-root,
  `read_only`, `cap_drop: ALL`, `no-new-privileges`, healthcheck.

browser ──HTTP──> eulalie (Axum, :8080)
                    ├── SQLite (sessions, projets, chapitres, favoris)
                    └── /data/uploads (fichiers EPUB/PDF persistés)

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
| `EULALIE_PUBLIC_URL` | (absent) | URL publique en production ; si elle commence par `https://`, les cookies de session deviennent `Secure` (recommandé derrière un reverse proxy TLS) |
| `EULALIE_DISABLE_REGISTRATION` | (absent) | si définie (ex. `=1`), les inscriptions sont désactivées : `/register` renvoie 404 et le lien disparaît de la page de connexion |

**Langues de traduction** : chaque projet a sa paire source→cible (par défaut en→fr),
modifiable dans l'en-tête de l'éditeur (sélecteurs + ↻). Le bouton « Traduire » utilise
les langues du projet courant. Langues disponibles : en, fr, de, es, it, pt, nl, ru, zh, ja, ar
(sous réserve des modèles chargés dans LibreTranslate : `--load-only en,fr` par défaut).
| `RUST_LOG` | `eulalie=info` | verbosité des logs |

### Production

En production, exposez derrière un reverse proxy TLS (Caddy, nginx, Traefik) :
le port n'est publié que sur `127.0.0.1` par défaut dans le compose. Définissez
alors `EULALIE_PUBLIC_URL=https://votre-domaine` pour activer les cookies
`Secure`. En HTTP local, aucun réglage n'est nécessaire : les cookies
fonctionnent sans `Secure` (comportement par défaut).

## Sécurité (référentiel OWASP)

Mesures implémentées, point par point :

### A01 Broken Access Control
- Chaque requête sur un projet/chapitre vérifie `project.user_id = session.user_id`
  (testé : un utilisateur B accédant au projet de A reçoit 404).
- Anonyme → redirection `/login` (pas de fuite de contenu).

### A02 Cryptographic Failures
- Mots de passe hachés en **Argon2id** (recommandation OWASP Password Storage).
- Cookies de session `HttpOnly` + `SameSite=Strict` + `Secure` automatique si `EULALIE_PUBLIC_URL` est en HTTPS.

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

## Déploiement (Arcane, PaaS auto-hébergé)

### Option A — laisser Arcane construire depuis le dépôt Git
Arcane clone le dépôt et construit l'image lui-même : créez une application,
pointez-la sur `sklaxs/eulalie` (branche `main`). Arcane détecte le `Dockerfile`
à la racine. Pensez à :
- connecter l'application GitHub d'Arcane à votre dépôt privé (ses apps :
  *Repository access* → `sklaxs/eulalie`) ;
- déclarer le **port 8080** et monter un **volume sur `/data`** (sinon la base
  SQLite disparaît à chaque redéploiement).

### Option B — tirer l'image pré-construite (recommandé)
Le workflow GitHub Actions publie `ghcr.io/sklaxs/eulalie:main` à chaque push.
Dans Arcane, créez l'application avec l'image du registre, comme un conteneur
existant — pas de build sur le serveur. Si le paquet est privé, configurez
dans Arcane les identifiants du registre `ghcr.io` (votre utilisateur GitHub +
un token `read:packages`), ou rendez le paquet public.

Dans les deux cas, variables recommandées :
- `EULALIE_PUBLIC_URL=https://votre-domaine` — cookies `Secure` ;
- `EULALIE_DISABLE_REGISTRATION=1` — après création de votre compte ;
- volume `/data` **obligatoire** (SQLite + uploads).

LibreTranslate : image `libretranslate/libretranslate`, commande
`--load-only en,fr --disable-web-ui`, ~2 Go de RAM, port 5000 — déployez-le
comme deuxième application Arcane et pointez les Paramètres d'Eulalie dessus.

### docker-compose direct (sans Arcane)
Sur n'importe quel serveur avec Docker : `docker compose up -d --build`
(voir plus haut) — c'est la méthode de référence pour la machine locale.

L'image est construite et publiée automatiquement par GitHub Actions à chaque
push sur `main` : `ghcr.io/sklaxs/eulalie:main` (dépôt privé — pensez à rendre
le paquet visible dans *Package settings* si votre PaaS doit le tirer).

Sur votre PaaS, déployez ensuite comme n'importe quelle image de registre :

- **Image** : `ghcr.io/sklaxs/eulalie:main`
- **Port** : 8080
- **Variables** : `EULALIE_PUBLIC_URL=https://votre-domaine` (active les cookies
  `Secure`), éventuellement `EULALIE_DISABLE_REGISTRATION=1` après création du compte
- **Stockage persistant** : montez un volume sur `/data` (base SQLite + uploads)
  — indispensable, sans quoi tout est perdu au redéploiement
- **Mémoire** : 512 Mo suffisent pour Eulalie ; si vous co-déployez LibreTranslate,
  comptez 2 Go pour lui

Si votre PaaS ne peut pas tirer une image privée, deux options :
1. générer un *Personal Access Token* GitHub avec `read:packages` et l'utiliser
   comme identifiants de registre dans Arcane ;
2. ou rendre le paquet public (Package settings → Change visibility).

Le service LibreTranslate peut rester sur votre machine (compose local) ou être
déployé à côté — dans ce cas pointez Paramètres sur son URL.

## Développement

```bash
cargo run   # écoute sur :8080, crée eulalie.db
cargo test  # tests unitaires (extraction, auth, assainissement Markdown)
```

## Fonctionnalités récentes

- **Export Markdown sans titres** : seules vos traductions sont exportées,
  séparées par `---` ; les titres de chapitres/pages ne sont plus repris.
- **Images par projet** : bouton 🖼 dans l'éditeur → téléversez PNG/JPEG/GIF
  (magic bytes vérifiés) ; cliquez sur une image pour l'insérer en Markdown ;
  servies sur `/images/:id` avec contrôle de propriété par utilisateur.
- **Page Paramètres** (⚙ dans l'en-tête) :
  - sauvegarde automatique ON/OFF (si OFF, un bouton « 💾 Sauvegarder »
    apparaît dans la barre d'outils, même design que les autres) ;
  - raccourcis clavier personnalisables (chapitre précédent/suivant) ;
  - serveur LibreTranslate pour la traduction à la sélection.
- **Traduction EN→FR** : sélectionnez du texte dans le panneau de gauche,
  un bouton « 🌍 Traduire » apparaît près de la sélection ; la traduction
  est insérée au curseur. Passe par le serveur (endpoint `/translate`,
  timeout 10 s, contrôle de session) vers LibreTranslate — auto-hébergeable
  via le service optionnel du docker-compose.

## Limites

- PDF : extraction texte uniquement (pas de rendu page).
- Pas encore de rate limiting sur /login (à ajouter avec un reverse proxy,
  p. ex. `limit_req` nginx ou fail2ban).

## Icônes

Interface et favicon : icônes SVG [Tabler Icons](https://tabler.io/icons)
(licence MIT — libres d'utilisation, sans attribution requise), intégrées inline
(aucune requête vers un CDN externe).

## Licence

MIT

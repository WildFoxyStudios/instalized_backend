# Production deploy runbook — push notifications (FCM HTTP v1)

This is the step-by-step the user runs on their machine to take the
backend + Flutter app from "code complete" to "phone receives a real
push". It does **not** need code changes — only Firebase Console + 3
env vars + replacing one placeholder file.

> All commands assume `E:\FULL\` as the project root. Adjust paths for
> your machine. The agent that wrote this code (Mavis) cannot
> interactively log in to Firebase Console, so these steps are yours.

---

## 0. What you need from Firebase

| What | Where to get it | Where it lands |
|---|---|---|
| `google-services.json` for the Android app | Firebase Console → Project settings → Your apps → Android → `google-services.json` | `E:\FULL\app-flutter\android\app\google-services.json` (replace the placeholder) |
| Service-account JSON key for the backend | Firebase Console → Project settings → Service accounts → **Generate new private key** (Node.js or any flavor) | Anywhere on disk; the path goes into the `FCM_SERVICE_ACCOUNT_PATH` env var |
| The matching `project_id` | Same project settings page, top of "General" tab | `FCM_PROJECT_ID` env var |

The service account **must** have the `roles/firebase.messaging.admin`
role on the project; the generated key inherits it by default.

## 1. Android app: replace the placeholder `google-services.json`

```
E:\FULL\app-flutter\android\app\google-services.json
```

is currently a placeholder. Replace it with the real file. Then
re-run the Flutter Android build:

```pwsh
cd E:\FULL\app-flutter
flutter clean
flutter pub get
flutter run -d <device-id>
```

The first build will run the Firebase init — if `google-services.json`
is invalid, the build fails with a clear error pointing at the file.

## 2. Backend: env vars

Drop these into your shell (or your process manager / `.env` /
`fly.toml` `env:` block):

| Var | Required | Example |
|---|---|---|
| `FCM_SERVICE_ACCOUNT_PATH` | yes | `C:\secrets\hybridsocial-fcm.json` |
| `FCM_PROJECT_ID` | yes | `hybridsocial-prod` |
| `PUSH_WORKER_SECS` | no (default `5`) | `5` |
| `JWT_SECRET` | already required | (set elsewhere) |

The worker is **idle** when the env vars are missing — `tracing::info!`
emits "push: FCM not configured — worker disabled" at boot, and no
network calls go out. Set them and the next `cargo run` will start
shipping pushes.

## 3. Test the loop end-to-end

### 3.1 Boot the backend

```pwsh
cd E:\FULL\backend-rust
$env:FCM_SERVICE_ACCOUNT_PATH = 'C:\secrets\hybridsocial-fcm.json'
$env:FCM_PROJECT_ID = 'hybridsocial-prod'
cargo run
```

You should see in the log:

```
push: FCM client loaded from C:\secrets\hybridsocial-fcm.json
pinning: worker started
push: FCM client loaded …
```

### 3.2 Boot the Flutter app, sign in

The first login triggers `PushService.I.init()` which:
1. Calls `Firebase.initializeApp()` (the `google-services.json` from §1).
2. Calls `FirebaseMessaging.instance.getToken()` and gets a token.
3. POSTs the token to `/v1/push/register` (the request is authed with
   the user's JWT — no body parameters you need to think about).

The backend persists the token in `push_tokens`.

### 3.3 Trigger a notification

In another account, like/comment/follow the signed-in user. The
backend:

1. Persists a row in `notifications` (this happens today — your tests
   in `tests/social_v2.rs::notif_prefs_accept` already exercise it).
2. The push worker polls every 5 s, finds the row, looks up the
   signed-in user's push tokens, and POSTs to FCM HTTP v1 with:
   - `notification.title` = "hybridsocial"
   - `notification.body` = the headline ("Someone liked your post", etc.)
   - `data` = `{ notification_id, kind, post_id? }`
3. FCM forwards the message to the device. The Flutter app's
   `onMessage` callback is wired to show a system notification (via
   `flutter_local_notifications` — if you've added it; the FCM
   integration in `lib/core/push.dart` already logs the payload so
   you can verify the loop without it).

### 3.4 Verify in the DB

```sql
SELECT id, push_sent_at, push_attempts, push_last_error
FROM notifications
ORDER BY created_at DESC
LIMIT 5;
```

`push_sent_at` should be set to a recent epoch; `push_attempts` should
be 1; `push_last_error` should be `NULL`. If you see
`push_last_error` populated, the worker is logged-erroring the FCM
response — check the backend log for the status code.

## 4. Disabling pushes

There are three layers, from softest to hardest:

1. **User-level** — in the Flutter app, sign in → Settings →
   Notifications → toggle off individual kinds (or "Pause all"). The
   `user_notif_prefs` row drives the worker's per-kind gate.
2. **Token-level** — calling `POST /v1/push/unregister` from the
   client on logout. The Flutter app already does this in
   `Session.logout()`.
3. **Server-level** — remove `FCM_SERVICE_ACCOUNT_PATH` and
   `FCM_PROJECT_ID` from the env. The worker goes idle, no network
   calls.

## 5. Common failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `push: FCM init failed, worker will be disabled` at boot | service-account JSON is invalid or the project_id doesn't match | Re-download the key from Firebase Console, set both env vars |
| `fcm http 401` in the log | the service account doesn't have `firebase.messaging` permission | Re-generate the key (it inherits the role by default) |
| `fcm http 404 status=UNREGISTERED` | the app was uninstalled, or the token expired | the worker soft-deletes the row in `push_tokens` with `deactivated_reason='UNREGISTERED'`. The user re-installs and the next launch re-registers a fresh token |
| `fcm http 400 status=INVALID_ARGUMENT` | the token is malformed | the same as above — soft-delete |
| `push: FCM not configured — worker disabled` | env vars not set | set them per §2 |
| `unable to open asset: android/app/google-services.json` at Flutter build | the placeholder wasn't replaced | replace per §1 |
| Phone doesn't get the push but `push_sent_at` is set | the device's FCM token is stale, or `user_notif_prefs.pause_all=1` | check `user_notif_prefs` for that user; re-register on the device |

## 6. Quick local-only smoke test (no real FCM)

If you want to confirm the worker is reading notifications without
shipping a real push:

```pwsh
cd E:\FULL\backend-rust
# point the env at a non-existent key path:
$env:FCM_SERVICE_ACCOUNT_PATH = 'C:\does-not-exist.json'
$env:FCM_PROJECT_ID = 'fake'
cargo run
```

The worker logs `push: FCM init failed, worker will be disabled` and
stays idle. The DB still records notifications (and the WS hub still
fires `dm.new` / `notification.new` to live clients). The push
delivery path is the only thing that's disabled.

## 7. iOS / web

Out of scope for v1.0 (this PR). When you add them:

- **iOS** — generate a `GoogleService-Info.plist` in Firebase, drop it
  into `ios/Runner/`, add `firebase_messaging` `Darwin` initialization
  to the iOS AppDelegate. The backend already supports the iOS
  `platform: ios` token shape; nothing changes there.
- **Web** — add a Firebase web config snippet to `app.html`, call
  `firebase.initializeApp(...)` in the +layout's onMount. FCM web push
  requires the user to opt in via the browser permission prompt; the
  backend will start receiving web tokens once the user accepts.

---

If something doesn't match the description here, the source of truth
is the worker (`src/push/worker.rs`) and the client
(`app-flutter/lib/core/push.dart`). Both are short — read those first.

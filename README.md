<p align="center">
  <img src="assets/logo.svg" alt="Gateway LLM" width="112">
</p>

<h1 align="center">Gateway LLM</h1>

<p align="center">
  <a href="https://github.com/zbigniew73/gateway-llm/actions/workflows/ci.yml"><img src="https://github.com/zbigniew73/gateway-llm/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
</p>

<p align="center">
  <a href="#polski">Polski</a> · <a href="#english">English</a>
</p>

---

## Polski

Lekki gateway LLM napisany w Rust. Wystawia API zgodne z OpenAI i Anthropic na jednym porcie i kieruje żądania do różnych backendów.

### Funkcje

- **Dwa protokoły na jednym porcie:** OpenAI Chat Completions (passthrough) i Anthropic Messages (pełna translacja, w tym streaming SSE, narzędzia, bloki `thinking` i sekwencje stopu).
- **Aliasy modeli** z listą deploymentów, fallbackiem według kolejności i fallbackiem między aliasami (`fallback_model`).
- **Odporność:** cooldown deploymentu po serii błędów, limit żądań na minutę (RPM) per provider, osobne timeouty dla połączenia, streamu i odpowiedzi non-stream.
- **Zgodność z Claude Code:** liczba tokenów w streamingu (`stream_usage`) i szacunkowy endpoint `count_tokens`.
- **Bezpieczeństwo:** autoryzacja jednym kluczem (`x-api-key` lub `Authorization: Bearer`), domyślny nasłuch tylko na `127.0.0.1`.
- **Instalacja ze źródeł** jako usługa `systemd --user` — działa na dowolnej architekturze CPU.
- **Diagnostyka** instalacji i providerów: `gateway-llm doctor`.

### Endpointy

| Metoda | Ścieżka | Opis |
|---|---|---|
| `POST` | `/v1/chat/completions` | API zgodne z OpenAI, przekazywane do providera bez zmian |
| `POST` | `/v1/messages` | API zgodne z Anthropic, tłumaczone do i z formatu OpenAI |
| `POST` | `/v1/messages/count_tokens` | Szacunkowa liczba tokenów wejścia (ok. 4 bajty na token) |
| `GET` | `/healthz` | Stan usługi, bez autoryzacji |

Maksymalny rozmiar żądania: 32 MB.

### Wymagania

- Linux z systemd
- Rust stable (MSRV 1.85) — `install.sh` instaluje go automatycznie, jeśli go brak
- Klucze API wybranych providerów

### Instalacja

```bash
git clone https://github.com/zbigniew73/gateway-llm.git ~/gateway-llm
cd ~/gateway-llm
cp .env.example .env
$EDITOR .env
./install.sh
```

`install.sh` buduje binarkę (`cargo build --release`), ustawia uprawnienia `.env` na `600`, instaluje i uruchamia usługę `~/.config/systemd/user/gateway-llm.service` oraz włącza `linger`, aby usługa działała bez aktywnej sesji użytkownika.

### Konfiguracja

#### `.env`

| Zmienna | Opis |
|---|---|
| `GATEWAY_API_KEY` | Klucz, którym klienci autoryzują się do gatewaya (wymagany) |
| `OPENROUTER_API_KEY`, `NOVITA_API_KEY`, `INFRON_API_KEY`, `NVIDIA_API_KEY` | Klucze providerów, wskazywane przez `api_key_env` w `config.yaml` |
| `GATEWAY_CONFIG` | Opcjonalna ścieżka do konfiguracji (domyślnie `./config.yaml`, następnie `config.yaml` obok binarki) |
| `RUST_LOG` | Poziom logowania, np. `gateway_llm=info` |

#### `config.yaml`

```yaml
server:
  host: 127.0.0.1
  port: 4444

routing:
  connect_timeout_seconds: 20
  stream_idle_timeout_seconds: 90
  non_stream_timeout_seconds: 300
  error_threshold: 3
  error_window_seconds: 120
  cooldown_seconds: 60

providers:
  openrouter:
    base_url: https://openrouter.ai/api/v1
    chat_path: /chat/completions
    rpm: 20
    headers:
      HTTP-Referer: https://github.com/zbigniew73/gateway-llm
      X-OpenRouter-Title: Gateway LLM
  infron:
    base_url: https://llm.onerouter.pro
    chat_path: /v1/chat/completions
    rpm: 60

model_list:
  - model_name: cc-main
    fallback_model: cc-fallback
    deployments:
      - provider: openrouter
        model: inclusionai/ling-3.0-flash-vl:free
        api_key_env: OPENROUTER_API_KEY
        order: 1
        stream_usage: true
  - model_name: cc-fallback
    deployments:
      - provider: infron
        model: z-ai/glm-5.2
        api_key_env: INFRON_API_KEY
        order: 1
        stream_usage: true
```

**`routing`**

| Pole | Domyślnie | Opis |
|---|---|---|
| `connect_timeout_seconds` | `20` | Nawiązanie połączenia oraz oczekiwanie na nagłówki streamu |
| `stream_idle_timeout_seconds` | `90` | Maksymalna cisza między zdarzeniami trwającego streamu |
| `non_stream_timeout_seconds` | `300` | Pełny czas odpowiedzi bez streamingu |
| `error_threshold` | `3` | Liczba błędów w oknie, po której deployment trafia w cooldown |
| `error_window_seconds` | `120` | Okno zliczania błędów |
| `cooldown_seconds` | `60` | Czas pomijania deploymentu po przekroczeniu progu |

**`providers`**

| Pole | Domyślnie | Opis |
|---|---|---|
| `base_url` | wymagane | Bazowy URL API zgodnego z OpenAI |
| `chat_path` | wymagane | Ścieżka endpointu chat completions |
| `rpm` | brak limitu | Limit żądań na minutę wysyłanych przez gateway, wspólny dla wszystkich deploymentów providera |
| `headers` | brak | Dodatkowe nagłówki HTTP wysyłane do providera, np. `HTTP-Referer` i `X-OpenRouter-Title`, dzięki którym OpenRouter pokazuje aplikację jako „Gateway LLM” zamiast „Unknown” |

**`model_list`**

| Pole | Opis |
|---|---|
| `model_name` | Alias używany przez klientów w polu `model` |
| `fallback_model` | Opcjonalny alias próbowany, gdy zawiodą wszystkie deploymenty |
| `deployments[].provider` | Nazwa providera z sekcji `providers` |
| `deployments[].model` | Nazwa modelu u providera |
| `deployments[].api_key_env` | Zmienna środowiskowa z kluczem API |
| `deployments[].order` | Kolejność prób (rosnąco) |
| `deployments[].stream_usage` | Prosi o liczbę tokenów w streamie `/v1/messages` dla tego modelu (domyślnie `false`); włączać tylko dla modeli obsługujących `stream_options` |
| `deployments[].show_reasoning` | Zawsze przekazuje myślenie modelu (`reasoning_content`/`reasoning`) do `/v1/messages` jako bloki `thinking`, także gdy klient nie włączył thinking (domyślnie `false`) |

### Routing i fallback

1. Deploymenty aliasu są próbowane według `order`. Deploymenty w cooldownie są pomijane, a te bez wolnego limitu RPM trafiają na koniec kolejki. Gdy wszystkie są w cooldownie, gateway i tak próbuje każdego z nich.
2. Kolejny deployment jest próbowany po błędzie połączenia, timeoucie, HTTP `5xx`, `401`, `402`, `403`, `404`, `408`, `409`, `413`, `429` oraz po `400`/`422` oznaczającym przekroczone okno kontekstu modelu.
3. Pozostałe błędy `4xx` są zwracane klientowi od razu.
4. Gdy zawiodą wszystkie deploymenty, gateway próbuje `fallback_model`. Cykle między aliasami są odrzucane przy starcie.
5. Klient zawsze otrzymuje w odpowiedzi alias, którego użył w żądaniu.

### Klienci

**Claude Code** — zmienne środowiskowe albo `.claude/settings.json` (wzór: `.claude/settings.json.example`):

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:4444
export ANTHROPIC_AUTH_TOKEN=<GATEWAY_API_KEY>
export ANTHROPIC_MODEL=cc-main
```

`ANTHROPIC_AUTH_TOKEN` ma pierwszeństwo przed `ANTHROPIC_API_KEY` i logowaniem OAuth. Model wybrany w Claude Code musi odpowiadać aliasowi z `config.yaml`.

**Klienci zgodni z OpenAI:**

```text
base_url: http://127.0.0.1:4444/v1
api_key:  <GATEWAY_API_KEY>
model:    <alias z config.yaml>
```

### Diagnostyka

```bash
cd ~/gateway-llm
./target/release/gateway-llm doctor
./target/release/gateway-llm doctor --providers
```

| Tryb | Co sprawdza |
|---|---|
| `doctor` | `.env` i jego uprawnienia, poprawność `config.yaml`, `GATEWAY_API_KEY` i klucze providerów, usługę systemd i `linger`, odpowiedź `/healthz`, czy działająca usługa akceptuje klucz z `.env` |
| `doctor --providers` | Dodatkowo: jedno żądanie (`max_tokens: 5`) na każdy deployment oraz dla każdego modelu: obsługę `stream_options` w porównaniu z `stream_usage` i to, czy zwraca myślenie, w porównaniu z `show_reasoning` |

Wynik to lista `[ OK ]` / `[INFO]` / `[WARN]` / `[FAIL]` z podpowiedzią naprawy. Kod wyjścia `0` oznacza brak błędów `FAIL`.

### Zarządzanie usługą

```bash
systemctl --user status gateway-llm
systemctl --user restart gateway-llm
systemctl --user disable --now gateway-llm
journalctl --user -u gateway-llm -f
```

Aktualizacja:

```bash
cd ~/gateway-llm
git pull
cargo build --release
systemctl --user restart gateway-llm
```

### Rozwój

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI (GitHub Actions) uruchamia te same kroki przy każdym pushu na `main` i w pull requestach. Testy nie wymagają dostępu do sieci ani kluczy API.

---

## English

A lightweight LLM gateway written in Rust. It exposes OpenAI- and Anthropic-compatible APIs on a single port and routes requests to multiple backends.

### Features

- **Two protocols on one port:** OpenAI Chat Completions (passthrough) and Anthropic Messages (full translation, including SSE streaming, tools, `thinking` blocks and stop sequences).
- **Model aliases** with an ordered list of deployments, ordered fallback and cross-alias fallback (`fallback_model`).
- **Resilience:** per-deployment cooldown after repeated errors, per-provider requests-per-minute (RPM) limit, separate timeouts for connection, streaming and non-stream responses.
- **Claude Code compatibility:** token usage in streaming responses (`stream_usage`) and an estimated `count_tokens` endpoint.
- **Security:** single-key authentication (`x-api-key` or `Authorization: Bearer`), listens on `127.0.0.1` by default.
- **Built from source** and installed as a `systemd --user` service — runs on any CPU architecture.
- **Diagnostics** for the installation and providers: `gateway-llm doctor`.

### Endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/chat/completions` | OpenAI-compatible API, forwarded to the provider unchanged |
| `POST` | `/v1/messages` | Anthropic-compatible API, translated to and from the OpenAI format |
| `POST` | `/v1/messages/count_tokens` | Estimated input token count (about 4 bytes per token) |
| `GET` | `/healthz` | Service health, no authentication |

Maximum request size: 32 MB.

### Requirements

- Linux with systemd
- Rust stable (MSRV 1.85) — installed automatically by `install.sh` if missing
- API keys for the providers you use

### Installation

```bash
git clone https://github.com/zbigniew73/gateway-llm.git ~/gateway-llm
cd ~/gateway-llm
cp .env.example .env
$EDITOR .env
./install.sh
```

`install.sh` builds the binary (`cargo build --release`), sets `.env` permissions to `600`, installs and starts the `~/.config/systemd/user/gateway-llm.service` unit and enables `linger`, so the service keeps running without an active user session.

### Configuration

#### `.env`

| Variable | Description |
|---|---|
| `GATEWAY_API_KEY` | Key clients use to authenticate to the gateway (required) |
| `OPENROUTER_API_KEY`, `NOVITA_API_KEY`, `INFRON_API_KEY`, `NVIDIA_API_KEY` | Provider keys, referenced by `api_key_env` in `config.yaml` |
| `GATEWAY_CONFIG` | Optional configuration path (default `./config.yaml`, then `config.yaml` next to the binary) |
| `RUST_LOG` | Log level, e.g. `gateway_llm=info` |

#### `config.yaml`

See the example in the Polish section above — the file format is the same.

**`routing`**

| Field | Default | Description |
|---|---|---|
| `connect_timeout_seconds` | `20` | Connection setup and waiting for stream response headers |
| `stream_idle_timeout_seconds` | `90` | Maximum silence between events of an ongoing stream |
| `non_stream_timeout_seconds` | `300` | Full response time without streaming |
| `error_threshold` | `3` | Errors within the window before a deployment enters cooldown |
| `error_window_seconds` | `120` | Error counting window |
| `cooldown_seconds` | `60` | How long a deployment is skipped after crossing the threshold |

**`providers`**

| Field | Default | Description |
|---|---|---|
| `base_url` | required | Base URL of the OpenAI-compatible API |
| `chat_path` | required | Chat completions endpoint path |
| `rpm` | no limit | Requests per minute sent by the gateway, shared by all deployments of the provider |
| `headers` | none | Extra HTTP headers sent to the provider, e.g. `HTTP-Referer` and `X-OpenRouter-Title`, so OpenRouter lists the app as "Gateway LLM" instead of "Unknown" |

**`model_list`**

| Field | Description |
|---|---|
| `model_name` | Alias clients use in the `model` field |
| `fallback_model` | Optional alias tried when all deployments fail |
| `deployments[].provider` | Provider name from the `providers` section |
| `deployments[].model` | Model name at the provider |
| `deployments[].api_key_env` | Environment variable holding the API key |
| `deployments[].order` | Attempt order (ascending) |
| `deployments[].stream_usage` | Requests token usage in `/v1/messages` streams for this model (default `false`); enable only for models that support `stream_options` |
| `deployments[].show_reasoning` | Always forwards the model's reasoning (`reasoning_content`/`reasoning`) on `/v1/messages` as `thinking` blocks, even when the client did not enable thinking (default `false`) |

### Routing and fallback

1. Deployments of an alias are tried by `order`. Deployments in cooldown are skipped, and those without a free RPM token go to the end of the queue. If all of them are in cooldown, the gateway still tries each one.
2. The next deployment is tried after a connection error, a timeout, HTTP `5xx`, `401`, `402`, `403`, `404`, `408`, `409`, `413`, `429`, and after a `400`/`422` indicating the model's context window was exceeded.
3. All other `4xx` errors are returned to the client immediately.
4. When all deployments fail, the gateway tries `fallback_model`. Cycles between aliases are rejected at startup.
5. The response always carries the alias the client requested.

### Clients

**Claude Code** — environment variables or `.claude/settings.json` (template: `.claude/settings.json.example`):

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:4444
export ANTHROPIC_AUTH_TOKEN=<GATEWAY_API_KEY>
export ANTHROPIC_MODEL=cc-main
```

`ANTHROPIC_AUTH_TOKEN` takes precedence over `ANTHROPIC_API_KEY` and OAuth login. The model selected in Claude Code must match an alias from `config.yaml`.

**OpenAI-compatible clients:**

```text
base_url: http://127.0.0.1:4444/v1
api_key:  <GATEWAY_API_KEY>
model:    <alias from config.yaml>
```

### Diagnostics

```bash
cd ~/gateway-llm
./target/release/gateway-llm doctor
./target/release/gateway-llm doctor --providers
```

| Mode | Checks |
|---|---|
| `doctor` | `.env` and its permissions, `config.yaml` validity, `GATEWAY_API_KEY` and provider keys, the systemd service and `linger`, the `/healthz` response, whether the running service accepts the key from `.env` |
| `doctor --providers` | Additionally: one request (`max_tokens: 5`) per deployment and for each model: `stream_options` support compared with `stream_usage`, and whether it returns reasoning, compared with `show_reasoning` |

The output lists `[ OK ]` / `[INFO]` / `[WARN]` / `[FAIL]` lines with a fix hint. Exit code `0` means no `FAIL`.

### Service management

```bash
systemctl --user status gateway-llm
systemctl --user restart gateway-llm
systemctl --user disable --now gateway-llm
journalctl --user -u gateway-llm -f
```

Update:

```bash
cd ~/gateway-llm
git pull
cargo build --release
systemctl --user restart gateway-llm
```

### Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI (GitHub Actions) runs the same steps on every push to `main` and on pull requests. Tests need neither network access nor API keys.
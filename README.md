# gateway-llm

Lekki, lokalny LLM gateway (odpowiednik "lite" LiteLLM proxy) do użytku osobistego. Jeden binarz Rust wystawia na `http://127.0.0.1:4444` dwa kompatybilne wire-protokoły jednocześnie:

- `POST /v1/chat/completions` — OpenAI-compatible (dla dowolnego klienta/SDK mówiącego formatem OpenAI, np. pi.dev)
- `POST /v1/messages` — Anthropic Messages API-compatible (dla Claude Code i innych klientów Anthropic-style), z pełną translacją do/z formatu OpenAI. Rozumowanie modelu (`reasoning_content` / `reasoning` providera) trafia do odpowiedzi jako bloki `thinking` — tylko gdy klient włączy thinking w żądaniu, tak jak w API Anthropic
- `POST /v1/messages/count_tokens` — liczba tokenów wejścia, ale **szacunkowa** (ok. 4 bajty na token, obraz ryczałtem ~1600): backendy OpenAI nie mają endpointu do dokładnego liczenia

Backendy: OpenRouter, Novita.ai, Infron.ai, NVIDIA NIM — z routingiem po aliasach modeli i automatycznym fallbackiem między nimi.

## Instalacja

**Katalog docelowy: `~/gateway-llm` (katalog domowy użytkownika na maszynie docelowej)** — `install.sh` sam wykrywa swoją lokalizację i generuje unit systemd z tą ścieżką, ale rekomendowana/oczekiwana lokalizacja to właśnie katalog domowy, np. `/home/<user>/gateway-llm`, a nie zagnieżdżony gdzieś głębiej (np. `~/projekty_github/gateway-llm`).

1. Skopiuj/sklonuj to repo na maszynę docelową dokładnie do `~/gateway-llm`:
   ```bash
   git clone <adres-repo> ~/gateway-llm
   # albo: rsync -a ./gateway-llm/ user@host:~/gateway-llm/
   cd ~/gateway-llm
   ```
2. Skonfiguruj klucze:
   ```bash
   cp .env.example .env
   $EDITOR .env   # ustaw GATEWAY_API_KEY oraz klucze providerów, których faktycznie używasz
   ```
3. Dostosuj `config.yaml` — lista aliasów modeli i ich deploymentów (provider + model + zmienna env z kluczem, kolejność fallbacku).
4. Uruchom instalator:
   ```bash
   ./install.sh
   ```
   Skrypt: doinstaluje `rustup`/Rust jeśli brak, zbuduje binarkę (`cargo build --release`), zainstaluje i uruchomi usługę `systemd --user` (`~/.config/systemd/user/gateway-llm.service`), oraz włączy `linger`, żeby usługa działała także bez aktywnej sesji logowania.

## Zarządzanie usługą

```bash
systemctl --user status gateway-llm
journalctl --user -u gateway-llm -f
systemctl --user restart gateway-llm
systemctl --user disable --now gateway-llm   # zatrzymanie/wyłączenie
```

## Aktualizacja

```bash
cd ~/gateway-llm
git pull
cargo build --release
systemctl --user restart gateway-llm
```

## Użycie z klientami

**Claude Code:**
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:4444
export ANTHROPIC_AUTH_TOKEN=<GATEWAY_API_KEY z .env>
claude
```
(`ANTHROPIC_AUTH_TOKEN` zamiast `ANTHROPIC_API_KEY` — wysyła `Authorization: Bearer` i ma wyższy priorytet niż ewentualny, osobno ustawiony prawdziwy `ANTHROPIC_API_KEY`; gateway akceptuje oba nagłówki, ale to bezpieczniejszy wybór, żeby uniknąć konfliktu.)

**Dowolny klient/SDK OpenAI-compatible (np. pi.dev):**
```
base_url: http://127.0.0.1:4444/v1
api_key:  <GATEWAY_API_KEY z .env>
```

Auth: gateway akceptuje klucz zarówno w nagłówku `x-api-key` (tak wysyła Claude Code), jak i `Authorization: Bearer <klucz>` (klienci OpenAI-SDK).

**Automatyczna konfiguracja Claude Code** (zamiast ręcznego `export` przed każdym uruchomieniem): skopiuj `.claude/settings.json.example` do `.claude/settings.json` w projekcie, w którym chcesz używać gatewaya (albo do `~/.claude/settings.json`, żeby dotyczyło wszystkich projektów), i podmień `ANTHROPIC_AUTH_TOKEN` na wartość `GATEWAY_API_KEY` z Twojego `.env`. Claude Code odczyta te zmienne środowiskowe automatycznie przy starcie (dokładnie jak `export`) i użyje ich zamiast logowania OAuth — nie trzeba się wylogowywać.

## Konfiguracja (`config.yaml`)

Każdy wpis w `model_list` to alias modelu (tego używają klienci w polu `model`) z uporządkowaną listą deploymentów — gateway próbuje ich po kolei (`order`) i automatycznie przechodzi do kolejnego przy błędzie (5xx/429/timeout), z cooldownem po serii błędów (`routing.error_threshold` / `routing.cooldown_seconds`).

Opcjonalne pole `rpm: <liczba>` na providerze w sekcji `providers` włącza proaktywny rate limiter (token bucket) — gateway pilnuje, żeby NIE przekroczyć tego limitu żądań/min do danego providera (dzielonego przez wszystkie deploymenty, które go używają), zamiast reagować dopiero na 429. Bez `rpm` provider jest bez limitu z naszej strony:

```yaml
providers:
  openrouter:
    base_url: https://openrouter.ai/api/v1
    chat_path: /chat/completions
    rpm: 20   # np. limit darmowych modeli OpenRoutera
```

Pole `stream_usage: true` na providerze sprawia, że przy streamingu na `/v1/messages` gateway prosi providera o liczbę tokenów (`stream_options.include_usage`) i przekazuje ją Claude Code (potrzebna m.in. do automatycznego kompaktowania kontekstu). Domyślnie wyłączone — włączaj tylko dla providerów, którzy ten parametr obsługują, bo inny może odrzucić żądanie z błędem 400. Passthrough `/v1/chat/completions` nie jest tym objęty.

Opcjonalne pole `fallback_model: <inny-model_name>` na wpisie aliasu pozwala przejść na CAŁKIEM INNY alias, gdy wyczerpią się WSZYSTKIE `deployments` bieżącego (a nie tylko pojedynczy deployment — to już obsługuje `order`):

```yaml
model_list:
  - model_name: cc-main
    fallback_model: cc-fallback
    deployments:
      - provider: openrouter
        model: inclusionai/ling-3.0-flash-vl:free
        api_key_env: OPENROUTER_API_KEY
        order: 1
  - model_name: cc-fallback
    deployments:
      - provider: infron
        model: z-ai/glm-5.2
        api_key_env: INFRON_API_KEY
        order: 1
```

Klient zawsze dostaje z powrotem pole `model` z aliasem, którego użył w żądaniu (`cc-main`), niezależnie od tego, który deployment/alias faktycznie obsłużył zapytanie — identycznie jak przy zwykłym fallbacku między deploymentami. Cykle w `fallback_model` (np. A → B → A) są odrzucane już przy starcie (`config.validate()`).

Błędy 400 a fallback: błąd „przekroczone okno kontekstu modelu” (rozpoznawany po treści, np. `context_length_exceeded`, `maximum context length`) przechodzi do kolejnego deploymentu, a potem do `fallback_model` — inny model może mieć większe okno. Nie liczy się do cooldownu. Każde inne 400/4xx (wadliwe żądanie) wraca do klienta od razu, bez prób na kolejnych deploymentach i bez `fallback_model`.

# PR-Agent

AI-ревьюер для pull request'ов. На каждый PR публикует разбор: ключевые
проблемы в коде (с inline-комментариями), оценку усилий на ревью, метки.

Написан на Rust, модельный движок — [opencode](https://opencode.ai).
Ключей LLM в самом pr-agent нет: модели и доступ настраиваются в opencode.

## Зачем

- **review** — разбор PR: проблемные места, безопасность, тесты
- **describe** — переписывает заголовок и описание PR
- **improve** — то же, что review (находки могут содержать готовые фиксы)
- **ask "вопрос"** — ответ на вопрос по PR прямо в комментариях

Работает с GitHub, GitLab, локальными git-репозиториями и просто diff-файлами.

## Как устроено

pr-agent — обвязка: забирает данные PR, готовит агенту рабочую копию,
публикует результат. Сам анализ делает opencode-агент `pr-reviewer`
(только чтение: править файлы и ходить в сеть он не может). Повторный
пуш в тот же PR не плодит новые ревью — сессия идемпотентна по head SHA.

## Быстрое внедрение на GitHub

Нужен self-hosted runner с Linux и Docker.

**1.** Собери образ и положи в свой registry:

```sh
docker build -f docker/Dockerfile.rust -t registry.example.com/pr-agent .
docker push registry.example.com/pr-agent
```

**2.** Настрой runner: положи конфиг opencode (модели, провайдер, ключи —
[пример настройки](https://opencode.ai/docs/config)) в
`/home/runner/.config/opencode`.

**3.** Скопируй
[`examples/github-workflow-review.yml`](examples/github-workflow-review.yml)
в `.github/workflows/pr-agent.yml` и подставь свой образ и модель:

```yaml
env:
  PR_AGENT_IMAGE: registry.example.com/pr-agent@sha256:<digest>
  PR_AGENT_MODEL: ${{ secrets.PR_AGENT_MODEL }}   # например litellm/gpt-oss
```

**4.** Добавь секрет `PR_AGENT_MODEL`. Готово — каждый открытый PR
получает ревью.

Комментарии-команды (`/review`, `/describe`, `/ask ...`) — рядом, в
[`examples/github-workflow-commands.yml`](examples/github-workflow-commands.yml).
GitLab-варианты (CI-джоба и webhook-сервис) — в [`examples/`](examples/).

## Проверить локально

```sh
docker run --rm \
  -v ~/.config/opencode:/root/.config/opencode:ro \
  -e PR_URL=https://github.com/org/repo/pull/123 \
  -e GITHUB__USER_TOKEN=ghp_... \
  registry.example.com/pr-agent
```

Или без контейнера (`cargo build --release` в `rust/`):

```sh
pr_agent --pr_url https://github.com/org/repo/pull/123 review
pr_agent doctor   # проверка движка: бинарник + пинг модели
```

## Подробнее

- [Настройки](docs/configuration.md) — все переменные окружения и ключи
- [Движок](docs/engine.md) — как работает opencode, агент, сессии

## Разработка

```sh
cd rust
cargo test          # юнит-тесты + интеграционные CLI-тесты (самодостаточные)
cargo clippy --all-targets -- -D warnings
```

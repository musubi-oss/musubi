# Матрица паритета: Musubi против Pragma / Nakama / PlayFab / AccelByte

Снимок состояния бэкенда на `main` (октябрь 2026). Проверено по коду, не по планам: каждая строка
со статусом ✅ или 🟡 указывает крейт/маршрут. Обозначения: ✅ есть · 🟡 частично · ❌ нет · — не
применимо. Колонки конкурентов — из части 2 плана (`game-backend-plan/02-matrica-i-dyry.md`),
Pragma Engine и AccelByte добавлены по их публичным фичлистам.

В конце — упорядоченная очередь работ: что закрываем, в каком порядке и почему.

## 1. Платформа

| Фича | Musubi | Pragma | Nakama OSS / Ent | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Лицензия, self-host | ✅ Apache-2.0, один бинарь | source-available, лицензия на студию | Apache-2.0 / Ent платно | SaaS | SaaS + enterprise |
| Кластер из коробки | ✅ `musubi-cluster` (SWIM + Raft-директория, шина) | ✅ | ❌ / ✅ | ✅ | ✅ |
| Мультирегион | ✅ регионы нод, размещение миров по региону (`worlds`) | 🟡 | ❌ / 🟡 | ✅ | ✅ |
| Геороутинг клиента к ближайшему региону | ✅ `GET /v1/regions` (echo/http-пробы, `suggested` по стране из CDN-заголовка), `ping_<region>` в тикете → регион матча → `allocate_routed` по пулу флотов, cold start при scale-to-zero | 🟡 | ❌ | ✅ | ✅ |
| Окружения dev → stage → prod с промоушеном конфига/контента | ✅ `node.environment` на кластере; бандлы по секциям `content` (валюты, предметы, награды, прогрессии, квесты, витрина, купоны, рейтинг, ачивки, крафт, сезоны, стрики, энергии, entitlements, IAP-продукты, лидерборды) и `config` (схемы коллекций, лимиты грантов, вебхуки, чат-фильтры): экспорт со штампом окружения, импорт с dry-run diff'ом, prune и проверкой целевого окружения, история версий и откат | ✅ (Pragma environments) | ❌ | ✅ | ✅ |
| Мультипроект (несколько игр на кластере) | ✅ `[[projects]]`: своя база и cluster group на проект в одном процессе, выбор по `x-musubi-project`/`Host`, `GET /v1/projects` | ✅ | ❌ | ✅ (titles) | ✅ (namespaces) |
| Скоуп-ключи сервер/админ | ✅ `server_keys` role server\|admin, аудит мутаций | ✅ | 🟡 один HTTP key | ✅ | ✅ |
| RBAC операторов консоли (роли, SSO операторов) | ✅ роли ключей server / admin / support / liveops / ops (чтение всем, запись по области, 403 с маршрутом); SSO операторов через любой OIDC (`[operators.sso]`: группы → роль, домены, короткоживущие ключи) | ✅ | ❌ / ✅ | ✅ | ✅ |
| Квоты и rate-limit per key / per account | ✅ лимиты auth/ip, http/rpc/socket per account с переопределениями и дневным бюджетом (`PUT /v1/admin/accounts/{id}/limits`), per server key (`[limits] key_*`, `PUT /v1/admin/keys/{id}/limits`), cluster-wide через шину | ✅ | ❌ | ✅ | ✅ |
| Бэкап / restore данных игрока точечно | ✅ export JSON (`/v1/admin/accounts/{id}/export`) + `POST …/restore {export, tables?, mode: replace|merge, create_missing}` — построчный restore тех же таблиц в одной транзакции, воссоздание purged-аккаунта | 🟡 | ❌ | ✅ | 🟡 |

## 2. Идентичность, данные, социалка

| Фича | Musubi | Pragma | Nakama | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Device / email+пароль / Steam / Google / Apple | ✅ `musubi-auth`, `http/auth.rs` | ✅ | ✅ | ✅ | ✅ |
| Generic OIDC (Discord, Twitch, Epic, Keycloak…) | ✅ `/v1/auth/oidc/{provider}`, discovery + JWKS | 🟡 | 🟡 | 🟡 | ✅ |
| Xbox Live (XSTS), PSN, Nintendo | ✅ `POST /v1/auth/{xbox,psn,nintendo}` + link/unlink (XSTS-профиль, id_token через OIDC-проверку) | ✅ | ❌ (custom) | ✅ | ✅ |
| Passkeys / WebAuthn | ✅ | ❌ | ❌ | ❌ | ❌ |
| TOTP MFA + recovery codes | ✅ | ❌ | ❌ | ❌ | 🟡 |
| Magic-link вход | ❌ (есть verify/reset по токену, вход по ссылке нет) | ❌ | ❌ | ✅ | ❌ |
| Lockout, captcha при брутфорсе, suspicious login | ✅ `[session]`, `hook_auth_captcha`, `account.suspicious_login` | 🟡 | ❌ | 🟡 | 🟡 |
| Login queue при лимите онлайна | ✅ `POST /v1/auth/queue` (гейт в выдаче сессии) | ✅ | ❌ | ❌ | 🟡 |
| Линковка / слияние аккаунтов | ✅ link/unlink, `POST /v1/account/merge` с журналом | ✅ | ✅ (link) | ✅ | ✅ |
| Сессии: refresh-семейства, revoke, device list, IP | ✅ | ✅ | ✅ | ✅ | ✅ |
| Отправка e-mail (verify/reset) | ✅ `[email]` SMTP (STARTTLS/implicit/none, AUTH PLAIN) в `musubi-integrations/email.rs`, события `email.sent/failed`; плюс событие в шину для своего мейлера | 🟡 | ❌ | ✅ | ✅ |
| Хранилище объектов (коллекции, права, версии, TTL) | ✅ `musubi-storage` | ✅ player data | ✅ | ✅ | ✅ |
| Запросы по полям | ✅ `list_where` (фильтр по JSON-полям) | 🟡 | 🟡 | ❌ | ❌ |
| Вторичные индексы / full-text | ✅ expression-индексы по полям `value` (`indexes` в контракте коллекции) + `$where` range-фильтры в `where`; full-text — нет | ❌ | 🟡 | ❌ | ❌ |
| Live-подписка на изменения объектов | ✅ `storage_subscribe` / `storage_update` в сокете | ❌ | ❌ | ❌ | ❌ |
| Схемы player data и миграции версий | ✅ `PUT /v1/admin/storage_schemas/{c}`: JSON Schema-контракт на коллекцию, версия + миграции (на чтении и backfill), live-подписки `storage_subscribe` по WS | ✅ | ❌ | ❌ | ❌ |
| Cloud save с разрешением конфликтов | ✅ версии + CAS; при устаревшей версии `merge: ours|theirs|max|min` + `base` в write-операции — трёхстороннее слияние на сервере (поле, изменённое одной стороной, побеждает; оба — по политике), ack с `merged.conflicts` | ❌ | 🟡 | ✅ | ✅ |
| Друзья, блок, рекомендации, недавние | ✅ `musubi-social` | ✅ | ✅ | ✅ | ✅ |
| Импорт друзей с платформ (Steam/PSN/Xbox) | ✅ `POST /v1/friends/import {provider, subjects}` (разово; периодического рефреша нет) | ✅ | ❌ | 🟡 | ✅ |
| Группы: роли-битмаска, казна, склад, XP/уровни, альянсы | ✅ | 🟡 | 🟡 | 🟡 | ✅ |
| Войны / челленджи кланов | ❌ (геймплейная логика поверх, ядро есть) | ❌ | ❌ | ❌ | ❌ |
| Чат: история, треды, реакции, правка, упоминания, модерация, slow-mode | ✅ `musubi-chat` | 🟡 | ✅ | 🟡 | ✅ |
| Перевод сообщений | ❌ | ❌ | ❌ | ❌ | ❌ |
| Голос | ✅ через Aurix (токены, позиционный канал мира, модерация) | ❌ | ❌ | ✅ | ❌ |
| Присутствие / статус | ✅ hub + шина (`online_remote`) | ✅ | ✅ | ❌ | ✅ |
| Пати, лобби, ready, команды | ✅ parties (home-node) + `/v1/lobbies` | ✅ | ✅ | ✅ | ✅ |
| Уведомления in-app | ✅ | ✅ | ✅ | ✅ | ✅ |
| Push FCM / APNs | ✅ `push.rs` | ❌ | ❌ | ✅ | ❌ |
| WebPush | ✅ `push.rs`: RFC 8030/8291 aes128gcm + VAPID, `platform: webpush` в `/v1/account/push_tokens` | ❌ | ❌ | ❌ | ❌ |
| UGC (моды, карты, скины) | ✅ `/v1/ugc`: реестр с версиями, blob ≤ 16 MiB или URL, теги/поиск/сортировка, рейтинг, подписки, модерация draft→pending→approved/rejected/hidden | ❌ | ❌ | ✅ | ✅ |
| GDPR export / delete с retention | ✅ | ✅ | ✅ | ✅ | ✅ |
| Возраст / родительский контроль / COPPA, приватность профиля | ❌ | ✅ | ❌ | 🟡 | ✅ |

## 3. Соревновательные и мета-системы

| Фича | Musubi | Pragma | Nakama (+Hiro) | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Лидерборды: операторы, сброс по cron, around | ✅ | 🟡 | ✅ | ✅ | ✅ |
| Лиги / дивизионы (promote/demote по тирам) | ✅ `League` в `musubi-leaderboard` | ❌ | ❌ | ❌ | ❌ |
| Турниры: окно, join_code, призы по рангам, командные | ✅ | 🟡 | ✅ | 🟡 | 🟡 |
| Брекеты (single/double elimination, swiss) | ✅ `POST /v1/server/tournaments/{id}/bracket`: сид по счёту/порядку/рандому/списку, byes, double elim с losers bracket, swiss без рематчей; результат через end-game report (`bracket_match`) | ❌ | ❌ | ❌ | ❌ |
| Рейтинги Elo / Glicko-2 / OpenSkill, сезоны, decay | ✅ `rating.rs` | 🟡 | ❌ | ❌ | 🟡 |
| Достижения, квесты, стрики, энергия, прогрессия | ✅ `musubi-economy` | 🟡 | Hiro платно | ✅ | ✅ |
| Сезоны / баттл-пасс | ✅ | ❌ | Hiro | ✅ | ✅ |
| Валюты, инвентарь, стор (окна, бандлы, гейты, скидки, купоны, ротация) | ✅ | ✅ | 🟡 wallet | ✅ | ✅ |
| Крафт | ✅ рецепты `PUT /v1/server/crafting/recipes` (входы: предметы/валюта; выходы: предметы/валюта/таблицы наград; requires_item, max_batch, лимит на аккаунт), `POST /v1/crafting/craft` атомарно + идемпотентно, под limited grants | ✅ | Hiro | ❌ | ❌ |
| Гача с pity и публичным seed (commit-reveal) | ✅ | ❌ | Hiro | ❌ | ❌ |
| Аукционы, P2P-трейд, ордербук | ✅ | ❌ | ❌ | ❌ | ❌ |
| IAP: Apple / Google / Steam, уведомления, отзыв | ✅ `iap.rs` | ✅ | ✅ | ✅ | ✅ |
| IAP: PSN / Xbox / Nintendo / Epic | ❌ | ✅ | ❌ | ✅ | ✅ |
| Подписки с продлением / grace | ✅ `iap_products.kind = subscription` + `grace_secs`, продление по чеку и Apple V2 / Google RTDN, `GET /v1/iap/subscriptions` | 🟡 | ✅ | ✅ | ✅ |
| Entitlements: платформенные покупки / DLC → предметы | ✅ `/v1/entitlements` (+`/sync` по Steam-ownership), серверный каталог/grant/revoke | ✅ | ❌ | ✅ | ✅ |
| Limited grants (лимит серверных грантов на игрока в окно) | ✅ `/v1/server/grant_limits`, `GET /v1/admin/accounts/{id}/grants` | ✅ | ❌ | ❌ | ❌ |
| Жалобы на игроков общего вида + ревью | ✅ `POST /v1/reports` на игрока/матч/UGC, очередь и решение `GET/POST /v1/admin/reports/{id}`, история по аккаунту | ✅ | ❌ | ✅ | ✅ |
| Баны (timed, shadow), kick, purge | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Античит: серверная валидация, EAC/BattlEye-интеграция | ✅ размер/частота ввода в authoritative-матчах → отказ + сигнал; `POST /v1/server/anticheat/flag` от серверов и `anticheat.flag` из модулей; очки за окно, авто-кик по порогу, очередь `GET /v1/admin/anticheat`; `POST /v1/server/anticheat/verify` → EAC/BattlEye endpoint | 🟡 | ❌ | 🟡 | 🟡 |

## 4. Realtime, матчмейкинг, серверы

| Фича | Musubi | Pragma | Nakama | PlayFab | AccelByte |
|---|---|---|---|---|---|
| WebSocket, QUIC, WebTransport, UDP, WebRTC DataChannel | ✅ `musubi-transport` (все пять) | — | WS | WS/UDP | WS |
| Relay-матчи, host migration по RTT | ✅ | ❌ | ✅ relayed | ✅ | ❌ |
| Authoritative-матчи (акторы), снапшоты + журнал, failover, fencing | ✅ | ❌ (game instances на DS) | ✅ (без failover) | 🟡 | ❌ |
| Spectator, запись и экспорт реплеев | ✅ | ❌ | ❌ | ❌ | ❌ |
| Детерминированный rollback | ❌ | ❌ | ❌ | ❌ | ❌ |
| Матчмейкер: MMR-окно по времени, пинг, роли, команды, кроссплей, приоритет, ready-check, dodge | ✅ `QueueConfig` | ✅ | 🟡 query | ✅ | ✅ |
| Backfill в живой матч / сервер | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Game flow: пати → очередь → инстанс → результаты → награды | ✅ `POST /v1/server/matches/{id}/results` — один отчёт: рейтинг-очередь, награды по рангу (под limited grants), квесты, XP, bracket турнира, шина; идемпотентно по матчу | ✅ | ❌ | 🟡 | ✅ |
| Dedicated-серверы: провайдеры | ✅ local, docker, kubernetes (Agones-совместимый манифест), firecracker | 🟡 внешние | 🟡 FleetManager API | ✅ | ✅ AMS |
| Внешние оркестраторы как провайдеры (Edgegap, GameLift) | ✅ `provider = edgegap` (deploy/status/stop) и `gamelift` (CreateGameSession/Describe/Terminate, SigV4) рядом с local/docker/firecracker/k8s/agones | — | ✅ | — | — |
| Автоскейл: буфер + прогноз по истории | ✅ `demand`/`scale` | ❌ | ❌ | ✅ | ✅ |
| Scale-to-zero флота | ✅ min 0 + холодный старт при аллокации (ожидание готовности), прогноз по истории | ❌ | ❌ | ✅ | ✅ |
| Build pipeline: canary / promote / rollback, vCPU-часы | ✅ | ❌ | ❌ | 🟡 | ✅ |
| SDK сервера: ready/health/labels/players/watch/shutdown | ✅ `/v1/gameserver/*` | ✅ | 🟡 | ✅ | ✅ |

## 5. Логика, LiveOps, инструменты

| Фича | Musubi | Pragma | Nakama | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Серверная логика | ✅ WASM-модули (`musubi-runtime`): rpc, before/after, hook_*, match_*, schedule_* | Kotlin-плагины | Go/Lua/JS | CloudScript | ❌ |
| Hot reload модулей | ✅ `PUT /v1/admin/runtime/modules/{name}` | ❌ | ❌ | ✅ | — |
| Канареечный rollout модулей | ✅ `/v1/admin/runtime/modules/{name}/canary` (процент/список аккаунтов, promote) | ❌ | ❌ | ❌ | — |
| Изоляция: fuel, память, дедлайн | ✅ | — | ❌ | ✅ | — |
| Локальная отладка: dev-сервер, фикстуры, replay запросов | ✅ `musubi dev`; фикстуры `POST /v1/admin/dev/fixtures`, запись/replay запросов `/v1/admin/dev/recording(s)`, канарейки модулей | ✅ | 🟡 | 🟡 | — |
| Cron / schedule_-хуки | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Durable workflows / saga | ❌ | ❌ | ❌ | ✅ | ❌ |
| Flags, live events, аудитории, A/B | ✅ через Zanshin (кэш + мгновенный пуш `liveops.changed`) | ✅ | Satori платно | ✅ | ✅ |
| Аналитика событий | ✅ шина → Zanshin / вебхуки; клиентскую телеметрию пишет Zanshin SDK | ✅ | Satori | ✅ | ✅ |
| Вебхуки: HMAC, backoff, dead-letter, redeliver, test | ✅ | 🟡 | ❌ | ✅ | ✅ |
| Аудит админ-мутаций | ✅ `/v1/admin/audit` | ✅ | ❌ | ✅ | ✅ |
| Админ-API: аккаунты (поиск по identity, patch, ban, sessions, export), storage, chat-модерация, worlds/matches, конфиг | ✅ | ✅ portal | 🟡 | ✅ | ✅ |
| Контент-каталоги с версиями / item definitions | ✅ контент-бандлы с diff/историей/откатом + версии item definitions (`instance_schema`, миграции инстансов lazy/batch, история, откат, конвертация предметов) | ✅ | ❌ | ✅ | ✅ |
| OpenAPI, CLI, миграция с Nakama | ✅ `openapi.json`, `musubi migrate-nakama` | 🟡 | 🟡 | ✅ | ✅ |
| IaC (Helm / Terraform) | ✅ Helm-чарт `deploy/helm/musubi` (ConfigMap TOML + секреты env, Service http/udp, HPA, PDB) и Dockerfile; Terraform — нет | — | 🟡 | — | — |
| Prometheus, OpenTelemetry, trace_id в ошибках | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Бенчи: criterion + A/B-ворота в CI, `musubi bench/soak` | ✅ | ❌ | ❌ | ❌ | ❌ |
| SDK клиентов | 🟡 Unreal-скаффолд (`sdk/unreal`), остальные нет | Unreal/Unity | большинство | Unreal/Unity/JS | Unreal/Unity |

## 6. Истинный серверный мешинг

Что есть (`musubi-mesh`, `realtime/world.rs`): авторитет по ячейкам, handover с буферизацией
ввода и групповой передачей (транспорт + пассажиры), backoff после таймаута Ack, интерес
(grid + per-client), co-locate, interact через RL, lag-compensation (`rewind`), split/merge по cost,
soft-cap очередь, hysteresis, time dilation, размещение по регионам, fencing персистенции по term,
восстановление после падения хоста, provenance при сборке шардов, attach/detach в протоколе.

Чего нет для «мир сквозной, всё реплицируется между серверами»:

| Компонент | Статус | Что это значит |
|---|---|---|
| Replication layer как отдельный слой (граф сущностей хранит и стримит состояние, симуляторы stateless) | ✅ | `replicas` в конфиге мира: хост каждый тик стримит всю ячейку (все сущности, покомпонентно, keyframe раз в `repl_keyframe_ticks`) на standby-ноды, те подтверждают тик (`replicas`/`mirrors` в инфо мира); смерть хоста — ячейка поднимается из зеркала на последнем реплицированном тике (эфемерные сущности включительно), без чтения снапшота/журнала; симулятор держит только lease и вход, состояние живёт в слое репликации |
| Кросс-граничные реплики (ghost/proxy сущностей соседних ячеек внутри каждого симулятора) | ✅ | хост ячейки каждый тик стримит приграничную полосу (`ghost_margin`) хостам соседей как read-only ghosts; в `view/{x}/{y}` они сливаются с owned; устаревание по тикам (`ghost_stale_ticks`) — `musubi-mesh/src/ghost.rs` |
| Репликация server↔server на уровне компонентов (дельты, приоритеты, bandwidth budget) | ✅ | ghost-поток — дельты по компонентам (`changed_rev`), приоритет по расстоянию до границы, бюджет байт на ячейку в тик (`ghost_budget_bytes`), keyframe раз в `ghost_keyframe_ticks`; handover строится на ghost'е принимающей ноды: уходят только компоненты новее её ревизии (`base_rev`) + буфер ввода, без ghost'а — `resend_full` и повтор полным состоянием |
| Единый entity id space + адресация через директорию для любого запроса из любой ячейки | ✅ | директория Raft |
| Персистентный entity streaming (мир живёт без игроков, выгрузка холодных ячеек) | ✅ | снапшоты/журнал; hibernate по отсутствию интереса, затем `unload_after_ticks` — персистентные сущности уходят в `world_cold_cells` и освобождают память, возвращаются при интересе/lease/спавне; `cold_cells` в инфо мира |
| Детерминизм и replay симуляции для отладки мешинга | ✅ журнал мутаций с provenance (`by: sim/tx/handover/spawn/decay/queue`, `sim`), `GET /v1/admin/worlds/{w}/history?entity&from_tick&to_tick`, `GET …/at?tick` (time machine), `POST …/verify` — детерминированная пересборка из снапшотов+журнала и diff с живым состоянием | replay матчей есть; replay мира с provenance — частично |
| Кросс-ячеечные транзакции (атомарный обмен предметами между сущностями разных серверов) | ✅ | `POST /v1/server/worlds/{w}/tx` — двухфазно по шине: prepare с проверкой версий компонентов и удержанием сущностей (запись 409, handover отложен), голоса хостов, commit/abort везде, таймаут по тикам, идемпотентно по tx id; локальный случай атомарно без шины |

## 7. Очередь работ

Порядок — по сочетанию «блокирует паритет с Pragma/AccelByte» × «нужно для истинного мешинга» ×
«нельзя сделать поверх модулем».

1. ~~**Платформенные аккаунты консолей**: Xbox Live XSTS, PSN id_token, Nintendo NSA; импорт друзей с платформ.~~ ✅
2. ~~**Entitlements**: Steam DLC/ownership, PSN, Xbox → предметы инвентаря, идемпотентный синк~~ ✅; IAP PSN/Xbox/Nintendo — **нет** (чеки только Apple/Google/Steam).
3. ~~**Жалобы на игроков** общего вида с уликами (match id, метка реплея), очередь ревью, решение с баном из той же ручки.~~ ✅
4. ~~**Limited grants** + per-key квоты и rate-limit.~~ ✅
5. ~~**End-game report**: один маршрут «результаты матча → рейтинги + награды + статы + квесты», идемпотентный по матчу (замыкает game flow Pragma).~~ ✅
6. ~~**Мешинг I — кросс-граничные реплики**~~ ✅ — непрерывный поток ghost-дельт соседних ячеек с приоритетами, бюджетом и keyframe.
7. ~~**Мешинг II — handover без пересылки**~~ ✅ — handover передаёт только компоненты новее ревизии ghost'а на принимающей ноде.
8. **Мешинг III — replication layer**: ✅ standby-реплики ячеек (`replicas`), поток репликации всей ячейки, захват из зеркала при смерти хоста без снапшота/журнала.
9. ~~**Мешинг IV — кросс-ячеечные транзакции и авто-выгрузка холодных ячеек**~~ ✅
10. **Окружения и версии контента/конфига**: ~~контент-бандлы с diff/историей/откатом~~ ✅; ~~версии item definitions с миграцией инвентарей~~ ✅; ~~конфиг кластера по окружениям~~ ✅ (`node.environment`, секция `config` бандлов, проверка окружения при импорте).
11. ~~**RBAC операторов**~~ ✅ (роли ключей); ~~SSO операторов~~ ✅ (OIDC → operator key); ~~мультипроект~~ ✅ (`[[projects]]`).
12. ~~**Подписки IAP** (продление, grace), WebPush, SMTP-провайдер~~ ✅.
13. ~~**Схемы player data и миграции**, live-подписки на объекты, вторичные индексы~~ ✅.
14. ~~**Брекеты турниров**, крафт, UGC~~ ✅.
15. ~~**Античит-сигналы**~~ ✅ (валидация ввода, флаги серверов/модулей, очки + авто-кик, EAC/BattlEye verify).
16. ~~**Dev-инструменты**: фикстуры, запись/воспроизведение запросов, канареечный rollout модулей, Helm~~ ✅.
17. ~~**Геороутинг**, scale-to-zero флотов, Edgegap/GameLift как провайдеры~~ ✅.

Клиентские SDK и GDK намеренно после: wire-протокол мира меняется пунктами 6–8, писать SDK до них —
переписывать дважды.

## 8. Глубина против спецификации (05-podsistemy) — аудит 2026-10-18

Строка «✅» в матрице означает «функция есть и покрыта тестом», не «реализована на всю глубину дизайн-дока».
Ниже — что спецификация обещает, а кода нет (или он тоньше). Порядок внутри раздела — по боли.

- **Производительность (6.1).** Замерено (8 vCPU, PG на той же машине, 200 клиентов): HTTP 2.3k/с, storage 3.5k/с,
  WS ping 39k/с, чат 1.1k/с. Цели v1 — 10k auth/RPC и 5k storage **на 1 vCPU**. Профилирования горячего пути
  (axum → sqlx → PG, пул соединений, пайплайнинг) ещё не было; это самый большой разрыв между обещанием и фактом.
- ~~**Статистика игрока (5.6).**~~ ✅ `stat_defs` (sum/max/min/last, public/friends/private), `/v1/server/stats/*`,
  `/v1/stats[/{account}]` со срезами по scope + глобальным, `top`, фолд из `stats_scope` отчёта матча.
- **Лидерборды (5.6).** ~~Нет запросов by friends / by group / by rank range и истории прошлых окон как API~~ ✅
  `records?friends=1|group_id=|rank_from=&rank_to=`, `GET /v1/leaderboards/{id}/windows` (+ `expiry=` на прошлое окно).
  Турниры: ~~нет round-robin и таймаутов неявки в брекете~~ ✅ `format: round_robin`, `deadline_secs` на готовые матчи,
  `POST …/bracket/matches/{mid}/forfeit` после дедлайна.
- **Матчмейкер.** ~~Нет p50/p95 ожидания по очереди/региону и симулятора очереди~~ ✅ `GET /v1/server/matchmaker/stats`
  отдаёт `wait` (p50/p95/p99/max по очереди×региону, скользящее окно 2048 тикетов) и `waiting_ages`;
  `POST /v1/server/matchmaker/simulate` гоняет синтетику (MMR, пинги по регионам, пати) через живой конфиг очереди на виртуальном времени.
- **Чат и уведомления (5.5).** Нет typing-индикатора, вложений, перевода, внешнего automod-хука; у in-app
  уведомлений нет TTL; push без шаблонов/расписания по таймзоне/тихих пушей/deep-link, нет Huawei;
  mailbox не умеет рассылку по аудитории (только все / список).
  ✅ typing (`channel_typing` → `ChannelTyping`, 1/2 с на канал, без хранения), вложения (`content.attachments` ≤10,
  https/ugc-ссылки, размер ≤25 МиБ — контракт проверяется сервером), внешний automod-вебхук (`chat.automod_url`:
  allow/block/replace, таймаут, fail-open/closed). ✅ TTL in-app уведомлений (`ttl_secs` → `expires_at`, скрытие и очистка), шаблоны push (`{{var}}`, `{{username}}`),
  рассылка списку/всем с токеном, расписание по UTC или по локальному времени получателя (`accounts.timezone`,
  раскатка по часовым поясам, лидерский цикл 30 с). Остались: перевод чата, Huawei push, тихие пуши.
- **Присутствие (5.4).** Статус — одна строка; нет `appear_offline` и структурированного rich presence.
  Импорт платформенных друзей разовый, без периодического рефреша. Войны/челленджи кланов — нет.
  ✅ `status_update {status, rich, appear_offline}`: rich presence — JSON-объект ≤2 КБ рядом со статусом; appear_offline —
  подписчики получают leave и не видят сессию в снапшотах `status_follow`, сервер по-прежнему знает, что она онлайн.
- **Экономика (5.11).** Нет графов прогресса / skill trees, unlockables с таймерами и ускорением,
  rerolls заданий, модификаторов наград (x2 выходные), витрин per-платформа, синка достижений в
  Steam/PSN/Xbox, фрод-эвристик по скорости покупок. IAP: только Apple/Google/Steam (нет Epic, PSN, Xbox,
  Nintendo, Huawei, Samsung, Facebook).
  ✅ витрины per-платформа: `platforms` у листинга (пусто = везде), `GET /v1/store?platform=`.
- **Аутентификация (5.1).** Нет magic-link, custom token, лимита одновременных сессий на аккаунт, SAML.
- **Storage (5.3).** Patch — только JSON Merge Patch (нет RFC 6902), нет aggregate и full-text.
- **Оркестрация (5.9).** Нет провайдеров `gamefabric` и `custom (WASM)`, билды не в OCI-реестре, warm-pool
  «≤ 2 с до готовности» не измерен.
- **CLI (5.15).** Есть dev/migrate/migrate-nakama/export/import/doctor/config/bench/soak; нет `deploy`,
  `module publish/rollout/rollback`, `schema migrate`, `fleet push`, миграции с PlayFab/UGS, Terraform.
- **Realtime (5.8).** Типизированная схема состояния с автодельтами и детерминированный rollback — v2 / SDK.

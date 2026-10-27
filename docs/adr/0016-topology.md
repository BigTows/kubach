# 0016 — Топология: граф связей из API, без агента

Статус: принято

## Контекст
Эпик #321 — карта того, как компоненты связаны и общаются: вход (Ingress, Gateway API) →
Service → workload/Pod → ConfigMap/Secret/PVC и внешние адреса; поверх — сетевые политики
(#325, что разрешено) и живой трафик (#326, что реально ходит). Экран (#324) рисует граф
компонентом `kubach-ui` (#323), MCP-инструмент и экспорт (#327) читают его же. Нужна одна
модель без UI, которую все они разделяют. Ставить агента в кластер нельзя (ADR 0004), а
трафик между подами из API не виден.

## Решение
- **Модель — `kubach-core::topology`**, без GPUI: `TopologyInput` (снимки объектов по
  виду, `SourceKind`) → `build`/`TopologyBuilder` → `TopologyGraph` (узлы, рёбра,
  диагностика). Чистая функция от снимков: тестируется на YAML-фикстурах, не на кэше.
- **Источник структуры — только API**, через существующие watch `ResourceCache`
  (`TopologySource` — тонкий слой: `subscribe_many` на каждый вид, который отдаёт discovery,
  снимки → builder). Своих запросов нет. ConfigMap, Secret и ServiceAccount смотрятся только
  по метаданным (`WatchFilter::metadata_only`): нужны имена, а не данные; значения Secret
  в модель не попадают никогда. Gateway API читается как `DynamicObject` (типизированных
  нет в `k8s-openapi`); нет CRD — виды попадают в `unavailable`, граф строится без них.
- **Источник трафика (#326) — только Prometheus** (ADR 0009), если там есть метрики mesh/CNI
  (Istio, Linkerd, Cilium/Hubble). Своего сбора нет. Плагины источников, определение, опрос и
  память — [ADR 0017](0017-traffic-layer.md).
- **Узлы**: Ingress; Gateway, HTTPRoute, GRPCRoute, TLSRoute; Service (ClusterIP, headless,
  NodePort, LoadBalancer, ExternalName); workload — Deployment (вместе со своими
  ReplicaSet), StatefulSet, DaemonSet, CronJob (вместе со своими Job), ReplicaSet и Job без
  контроллера, а также контроллер, известный только по `ownerReferences` (Argo Rollout,
  оператор) — `WorkloadKind::Other`; Pod; ConfigMap, Secret, PVC, ServiceAccount — только
  те, на которые есть ссылки (все — по `include_unreferenced`); внешние адреса
  (ExternalName, IP/hostname балансировщика у Service, Ingress, Gateway). EndpointSlice не
  узел, а источник связей и готовности. Группировка — по namespace.
- **Детализация**: `Detail::Workloads` (по умолчанию) схлопывает поды в верхнего владельца;
  `Detail::Pods` показывает поды с ребром `Owns` от workload. Ссылки пода (volumes, env)
  висят на том узле, которым под нарисован; у workload без подов на графе — из pod template.
- **Рёбра** типизированы (`EdgeKind`): `RoutesTo` (Ingress/route → Service, Gateway → route;
  host, path, port, listener), `Selects` (Service → workload/Pod/адрес; ready/not ready из
  EndpointSlice, без них — из условия `Ready` пода), `Owns`, `Mounts` (volume, projected,
  imagePullSecrets, volumeClaimTemplates), `EnvRef` (`env.valueFrom`, `envFrom`),
  `UsesServiceAccount` (кроме `default`), `Exposes` (→ внешний адрес), `Tls`. Между двумя
  узлами не больше одного ребра вида; правила и ссылки копятся в его `EdgeAttrs`.
  ConfigMap `kube-root-ca.crt` из projected-тома токена не связывается (он у каждого пода).
- **Стабильные id** — из имён, не UID: `Kind[.group]/namespace/name`
  (`Deployment.apps/shop/web`, `External//203.0.113.10`), ребро — `kind:from->to`. Id не
  меняется при пересборке, пересоздании объекта и смене детализации (workload остаётся
  тем же узлом), а у объекта, на который ссылаются, но которого нет, — тот id, который был
  бы у него. По id UI сохраняет раскладку и выделение, а слои цепляются к узлам.
- **Расширяемость под слои**: новые виды рёбер — варианты `EdgeKind` (`#[non_exhaustive]`),
  новые факты — поля `EdgeAttrs`/`NodeAttrs` (все `Default`); слой добавляет свои рёбра
  `TopologyGraph::with_edges`. У узлов workload/Pod есть `pod_labels` — то, что матчат
  селекторы Service и NetworkPolicy.
- **Диагностика**: Service без готовых endpoints; селектор Service не совпадает ни с одним
  подом и ни с одним pod template; Ingress/route ссылается на несуществующий Service, порт
  или Gateway; ссылка на несуществующий ConfigMap/Secret/PVC/ServiceAccount или TLS Secret.
  Недостающая цель — узел с `Presence::Missing`. Отсутствие считается ошибкой, только если
  список вида полный (`TopologyInput::set`, watch загружен без ошибок); вид, который не
  загружен (нет прав, грузится, не обслуживается), даёт `Presence::Unknown` без диагностики.
  `optional: true` — `Missing` без диагностики.
- **Производительность**: объекты читаются прямо из JSON (`serde_json::Value`), без
  типизированной десериализации. `TopologyBuilder` запоминает факты каждого объекта по
  `resourceVersion` и при пересборке читает заново только изменившиеся; сама сборка
  (join) идёт целиком — она дешёвая: селекторы Service ищутся по индексу
  `(namespace, label, value)`. Замер (тест `three_thousand_pods_rebuild_quickly`: 30
  namespace × 10 Deployment × 10 подов = 3000 подов, 300 Service/EndpointSlice/ConfigMap/
  Secret, 30 Ingress; Apple Silicon): release — первая сборка ~15 мс, повторная ~5 мс,
  по подам ~15 мс; debug — ~50 / ~35 / ~80 мс. Diff на уровне графа (`TopologyGraph::diff`)
  — для UI, которому нужно знать, что добавилось/исчезло.

- **Слой сетевых политик (#325)** — `topology::netpol`, чистые функции без UI: разбор
  `networking.k8s.io/v1` NetworkPolicy из JSON (`Policy`, полный `LabelSelector` с
  `matchExpressions`, `ipBlock`/`except`, порты number/name/`endPort`, TCP/UDP/SCTP;
  `policyTypes` по умолчанию — как у API server: Ingress, плюс Egress при egress-правилах) и
  `PolicySet` с лейблами Namespace (`kubernetes.io/metadata.name` есть всегда). Семантика
  стандартная: под без политик направления не изолирован; выбранный хоть одной — только
  объединение правил, пустой список — deny all; соединению нужны egress источника и ingress
  цели; в одном пире `podSelector` и `namespaceSelector` — AND, разные пиры — OR; `ipBlock` с
  подами не сопоставляется (их IP зависят от реализации); именованный порт — по
  `containerPorts` пода-получателя (`NodeAttrs::container_ports`). Группа подов — namespace,
  лейблы и порты: узел workload или пода. `can_reach(from, to, port, protocol) → Verdict` с
  объяснением каждой стороны (какие политики изолируют, какие разрешили) и подсказкой про
  DNS, если egress закрыт и kube-dns не разрешён. Слой на графе (`netpol::apply`,
  `BuildOptions::network_policies`): `NodeAttrs::network_policy` (изоляция по направлениям),
  `EdgeAttrs::policy` на `Selects` (Service → поды на его target-портах: `Open`/`Allowed`/
  `Restricted`/`Blocked`) и рёбра `PolicyAllows` от workload, которые правило явно впускает
  (непустой `podSelector`), если egress источника выпускает. CRD Cilium/Calico не читаются;
  применяет ли политики CNI — неизвестно, это анализ объектов.
- **Подписка слоя**: NetworkPolicy (по namespace) и Namespace — `SourceKind::LAYERS`, в
  граф не рисуются; `TopologySource` держит их watch всегда: политик мало, watch Namespace уже
  держит выбор namespace, и слой включается без загрузки. Ошибки этих видов (например, нет
  прав на список Namespace) показываются только при включённом слое; без лейблов Namespace
  `namespaceSelector` сопоставляется только по `kubernetes.io/metadata.name`.

- **Экспорт и MCP (#327)**: Mermaid (`flowchart LR`, подграфы — namespace) и Graphviz DOT —
  чистые функции `topology::export` от готового графа (фильтры, детализация и слой политик уже
  применены); SVG рисует `kubach-ui` по координатам раскладки `GraphView` в цветах текущей темы,
  PNG — растеризация этого SVG через `resvg` (уже в дереве через GPUI). MCP-инструменты
  `get_topology` и `can_reach` строят граф не из UI-кэша, а одноразовым list каждого вида
  клиентом сессии в её namespace (ADR 0005): виды, которые политика сессии или API не дали,
  остаются неполными (`Presence::Unknown`) и перечисляются агенту; ConfigMap и Secret — только
  метаданные, Secret — лишь при «Secrets for the agent» не ниже метаданных.
- **Холст (#383)**: наведение только слегка подсвечивает узел и его рёбра, остальное не
  затемняется и состояние экрана не меняется; цепочка связей с затемнением остального —
  у выбранного кликом узла (повторный клик, Esc или клик по пустому месту снимают выбор).
  Подписи рёбер раскладываются чистой функцией `graph::labels` (общей для холста и SVG/PNG):
  на свободном участке ребра, вне карточек узлов и не поверх других подписей; не влезает —
  отбрасываются хвостовые части через « · », затем обрезка с «…», полный текст — в подсказке
  при наведении на ребро или подпись (в SVG — `<title>`).

## Последствия
- Экран топологии держит ~18 watch (по одному на вид и namespace); часть уже открыта
  другими экранами и разделяется, отпущенные живут `LINGER` (ADR 0012).
- Связи «кто с кем реально общается» без Prometheus с метриками mesh/CNI недоступны: граф
  показывает только объявленные связи (маршруты, селекторы, ссылки).
- Нет ссылок через аннотации и CRD, кроме Gateway API (cert-manager, ExternalSecret,
  ingress-контроллеры с собственными CRD) — их можно добавлять как новые `SourceKind` и
  рёбра.
- Селектор в фильтре — только равенство и существование (`app=web,tier!=db,canary,!x`),
  без `in`/`notin`.

# Architecture Decision Records

Короткие записи принятых решений (контекст — задача #3). Формат: контекст, решение, последствия.

- [0001 — GPUI для десктопного UI](0001-gpui.md)
- [0002 — kube-rs для ядра](0002-kube-rs.md)
- [0003 — Только встроенные плагины](0003-in-tree-plugins.md)
- [0004 — Только локальная работа, без агента в кластере](0004-local-only.md)
- [0005 — Права MCP-агента на namespace](0005-mcp-permissions.md) (принято)
- [0006 — Helm: чтение нативно, изменения через `helm` CLI](0006-helm.md)
- [0007 — Kustomize: сборка через `kustomize` или `kubectl kustomize`](0007-kustomize.md)
- [0008 — Зашифрованное хранилище импортированных kubeconfig](0008-kubeconfig-vault.md)
- [0009 — Метрики: metrics-server сейчас, Prometheus для истории](0009-prometheus-metrics.md)
- [0010 — Применение изменений: SSA и merge patch](0010-apply.md)
- [0011 — Async-рантайм и отмена задач](0011-async-runtime.md)
- [0012 — Кэш списков ресурсов на диске](0012-list-cache.md)
- [0013 — Быстрый старт: окружение shell в фоне и кэш discovery](0013-startup.md)
- [0014 — Разрывы watch и соединения с API](0014-watch-outages.md)
- [0015 — Локальный терминал: временный kubeconfig с текущим контекстом](0015-local-terminal-kubeconfig.md)
- [0016 — Топология: граф связей из API, без агента](0016-topology.md)
- [0017 — Слой трафика топологии: встроенные плагины источников](0017-traffic-layer.md)
- [0018 — Локализация интерфейса: Fluent, данные кластера не переводим](0018-i18n.md) (предложено)

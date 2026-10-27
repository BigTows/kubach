# 设计系统组件（kubach-ui）的内置文本。应用传入的名称和值（kind、状态、
# context 名称）是数据，保持原样。Kubernetes 术语保留英文。
# 机器翻译草稿，需要母语者审校。

## 通用

ui-cancel = 取消
ui-and-more = 还有 { $count } 项
ui-unavailable = 不可用

## 操作提示

ui-action-stop = 停止
ui-action-stopping = 正在停止…

## 批量操作

ui-bulk-running = 正在运行…
ui-bulk-done = 完成
ui-bulk-cancelled = 已取消
ui-bulk-failed = 失败
ui-bulk-succeeded = { $succeeded } 个成功
ui-bulk-succeeded-failed = { $succeeded } 个成功，{ $failed } 个失败
ui-bulk-hide-tooltip = 隐藏；它会在状态栏中继续运行
ui-bulk-hide-output = 隐藏输出
ui-bulk-show-output = 显示输出（{ $count }）

## 标签（labels 和 annotations）

ui-chip-click-to-copy = 点击复制
ui-chip-hide = 隐藏
ui-chip-show-less = 收起
ui-chip-show-all = 显示全部 { $count } 个
ui-chip-more = 还有 { $count } 个
ui-chip-copy-all = 全部复制

## 确认对话框

ui-confirm = 确认
ui-confirm-command = 命令
ui-confirm-copy-command = 复制命令
ui-confirm-type-to-confirm = 输入 { $text } 以确认。
ui-confirm-context = Context

## Diff

ui-diff-unchanged-lines = ⋯ { $count } 行未更改

## 列表搜索

ui-filter-no-matches = 没有与“{ $query }”匹配的 { $noun }
ui-filter-no-matches-description = 在名称的任意位置查找文本，不区分大小写。
ui-filter-clear-search = 清除搜索

## 流量图

ui-traffic-low = 低
ui-traffic-medium = 中
ui-traffic-high = 高
ui-traffic-degraded = 错误 ≥1%
ui-traffic-failing = 错误 ≥5%
ui-traffic-no-service = 无 Service
ui-graph-group-items = { $count } 项
ui-graph-zoom-out = 缩小
ui-graph-actual-size = 实际大小
ui-graph-zoom-in = 放大
ui-graph-fit = 适应视图
ui-graph-minimap = 小地图
ui-graph-laying-out = 正在布局…
ui-graph-click-to-pan = 点击后可平移和缩放

## 快捷栏图块颜色

ui-tile-blue = 蓝色
ui-tile-teal = 青色
ui-tile-orange = 橙色
ui-tile-purple = 紫色
ui-tile-pink = 粉色
ui-tile-gray = 灰色
ui-tile-custom = 自定义

## 图表

ui-chart-now = 现在
ui-chart-no-data = 无数据

## 日志

ui-log-no-lines = 暂无日志行
ui-log-new-lines = { $count } 行新日志

## 多选

ui-multi-none = 无
ui-multi-filter = 筛选…
ui-multi-all = 全部
ui-multi-select = 选择…
ui-multi-recent = 最近
ui-multi-use = 使用“{ $value }”
ui-multi-no-matches = 无匹配项
ui-multi-hint = 选择即生效 · ⌥-点击仅选一项

## 操作

ui-operations-title = 操作
ui-operations-empty = 暂无操作
ui-operations-empty-description = Helm 变更、drain 和批量操作在运行时显示在这里。
ui-operations-clear-finished = 清除已完成
ui-operations-notice-tooltip = 显示操作

## 表格和进度条

ui-replicas-ready = { $ready }/{ $desired } 就绪
ui-table-count-age = { $count }（{ $age } 前）
ui-table-reset-columns = 重置列顺序
ui-cubes-restarts = 重启次数：{ $count }
ui-side-list-empty = 这里什么都没有
ui-usage-allocatable = Allocatable
ui-usage-used = 已用
ui-usage-requests = Requests
ui-usage-limits = Limits

## Secret 与设置字段

ui-secret-choose-file = 选择文件…
ui-secret-no-file = 未选择文件
ui-secret-length = 长度
ui-secret-generate = 生成
ui-setting-reset = 恢复默认值
ui-setting-not-set = 未设置
ui-setting-choose = 选择…

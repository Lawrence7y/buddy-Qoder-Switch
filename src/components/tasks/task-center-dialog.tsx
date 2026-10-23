/**
 * 任务中心管理规范与执行器控制台（FUS-W02 / W06）。
 *
 * 交付范围与诚实边界：
 * - 统一 run/item 运行记录、持久化事件序列（sequence）、游标增量分页与跨进程账号锁互斥已在 core 与双宿主接口层实现并通过离线测试；
 * - 停止取消语义（未开始取消 / 已发写请求待确认）、取消幂等保护与失败重试过滤（排除待确认与已领项）已就绪；
 * - 遵循安全边界与 W05/W06 约束：当前仅使用离线安全 Handler / 临时数据库验证，未接触真实账号/网络，未启动桌面 GUI 或候选应用，桌面端真实点击/长耗时心跳/真实平台奖励尚未验收。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertCircle,
  Calendar,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Clock,
  Info,
  Loader2,
  Play,
  RefreshCw,
  RotateCcw,
  ScrollText,
  Square,
  Users,
  XCircle,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";
import type {
  ManagementTaskEvent,
  ManagementTaskItem,
  ManagementTaskRun,
  ManagementTaskSpec,
} from "@/lib/types";

interface TaskCenterDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  scope: string;
}

export function TaskCenterDialog({
  open,
  onOpenChange,
  scope,
}: TaskCenterDialogProps) {
  const [specs, setSpecs] = useState<ManagementTaskSpec[]>([]);
  const [runs, setRuns] = useState<ManagementTaskRun[]>([]);
  const [loading, setLoading] = useState(false);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [actionSuccessMsg, setActionSuccessMsg] = useState<string | null>(null);

  // 展开查看逐账号 item 详情与增量事件日志
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const [runItems, setRunItems] = useState<Record<string, ManagementTaskItem[]>>({});
  const [itemsLoading, setItemsLoading] = useState<string | null>(null);
  const [runEvents, setRunEvents] = useState<Record<string, ManagementTaskEvent[]>>({});
  const [eventCursors, setEventCursors] = useState<Record<string, number>>({});
  const [eventHasMore, setEventHasMore] = useState<Record<string, boolean>>({});
  const [eventsLoading, setEventsLoading] = useState<Record<string, boolean>>({});
  const [eventErrors, setEventErrors] = useState<Record<string, string | null>>({});
  const eventVersions = useRef<Record<string, number>>({});
  const contextKey = useRef("");
  const context = useRef(0);
  const nextContextKey = `${open}:${scope}`;
  if (contextKey.current !== nextContextKey) {
    contextKey.current = nextContextKey;
    context.current += 1;
  }
  const requestVersion = useRef(0);
  const actionBusy = useRef(false);

  const loadData = useCallback(async () => {
    if (!open) return;
    const expectedContext = context.current;
    const version = ++requestVersion.current;
    setLoading(true);
    setError(null);
    try {
      const [fetchedSpecs, fetchedRuns] = await Promise.all([
        api.getManagementTaskSpecs(scope),
        api.getManagementTaskRuns(scope),
      ]);
      if (context.current !== expectedContext || requestVersion.current !== version) return;
      setSpecs(fetchedSpecs);
      setRuns(fetchedRuns);
    } catch (err) {
      if (context.current === expectedContext && requestVersion.current === version) setError(api.asError(err));
    } finally {
      if (context.current === expectedContext && requestVersion.current === version) setLoading(false);
    }
  }, [open, scope]);

  useEffect(() => {
    setSpecs([]);
    setRuns([]);
    setRunItems({});
    setRunEvents({});
    setEventCursors({});
    setEventHasMore({});
    setEventsLoading({});
    setEventErrors({});
    eventVersions.current = {};
    setExpandedRunId(null);
    setItemsLoading(null);
    setActionSuccessMsg(null);
    setError(null);
    if (open) {
      void loadData();
    }
    return () => { requestVersion.current += 1; };
  }, [open, scope, loadData]);

  // 手动触发任务
  const handleTriggerTask = async (taskId: string) => {
    if (actionBusy.current) return;
    actionBusy.current = true;
    const expectedContext = context.current;
    setActionLoading(`trigger-${taskId}`);
    setError(null);
    setActionSuccessMsg(null);
    try {
      const run = await api.triggerManagementTask(scope, taskId, "manual");
      if (context.current !== expectedContext) return;
      setActionSuccessMsg(`任务已触发：运行 ID ${run.runId}（状态：${run.status}）`);
      await loadData();
      if (context.current !== expectedContext) return;
      if (run.runId) {
        setExpandedRunId(run.runId);
        void loadItemsForRun(run.runId, true);
        void loadEventsForRun(run.runId);
      }
    } catch (err) {
      if (context.current === expectedContext) setError(api.asError(err));
    } finally {
      actionBusy.current = false;
      setActionLoading(null);
    }
  };

  // 取消任务
  const handleCancelTask = async (runId: string) => {
    if (actionBusy.current) return;
    actionBusy.current = true;
    const expectedContext = context.current;
    setActionLoading(`cancel-${runId}`);
    setError(null);
    setActionSuccessMsg(null);
    try {
      const run = await api.cancelManagementTask(scope, runId);
      if (context.current !== expectedContext) return;
      setActionSuccessMsg(`任务已请求停止：运行 ID ${run.runId} 进入「${run.status}」状态`);
      await loadData();
      if (context.current !== expectedContext) return;
      void loadItemsForRun(runId, true);
      void loadEventsForRun(runId);
    } catch (err) {
      if (context.current === expectedContext) setError(api.asError(err));
    } finally {
      actionBusy.current = false;
      setActionLoading(null);
    }
  };

  // 失败重试
  const handleRetryTask = async (runId: string) => {
    if (actionBusy.current) return;
    actionBusy.current = true;
    const expectedContext = context.current;
    setActionLoading(`retry-${runId}`);
    setError(null);
    setActionSuccessMsg(null);
    try {
      const run = await api.retryManagementTask(scope, runId);
      if (context.current !== expectedContext) return;
      setActionSuccessMsg(`重试请求已提交：运行 ID ${run.runId}，当前状态「${run.status}」`);
      await loadData();
      if (context.current !== expectedContext) return;
      void loadItemsForRun(runId, true);
      void loadEventsForRun(runId);
    } catch (err) {
      if (context.current === expectedContext) setError(api.asError(err));
    } finally {
      actionBusy.current = false;
      setActionLoading(null);
    }
  };

  // 展开加载逐账号 items
  const loadItemsForRun = async (runId: string, force = false) => {
    if (!force && runItems[runId]) return;
    const expectedContext = context.current;
    setItemsLoading(runId);
    try {
      const items = await api.getManagementTaskItems(scope, runId);
      if (context.current !== expectedContext) return;
      setRunItems((prev) => ({ ...prev, [runId]: items }));
    } catch (err) {
      if (context.current === expectedContext) setError(`加载逐账号明细失败：${api.asError(err)}`);
    } finally {
      if (context.current === expectedContext) setItemsLoading(null);
    }
  };

  // 展开增量加载生命周期事件日志（FUS-W06：单调游标增量补读，按sequence去重，防止迟到旧事件回退终态）
  const loadEventsForRun = async (
    runId: string,
    _options?: { reset?: boolean } | boolean
  ) => {
    if (eventsLoading[runId]) return;

    const expectedContext = context.current;
    const nextVer = (eventVersions.current[runId] ?? 0) + 1;
    eventVersions.current[runId] = nextVer;

    setEventsLoading((prev) => ({ ...prev, [runId]: true }));
    setEventErrors((prev) => ({ ...prev, [runId]: null }));

    try {
      // 保持已有日志增量追读，禁止清空或丢失中段历史；只有当前 run 在该会话尚未加载过时才从 null 开始
      let accumulated: ManagementTaskEvent[] = runEvents[runId] ?? [];
      let cursor: number | null =
        eventCursors[runId] ??
        (accumulated.length > 0
          ? Math.max(...accumulated.map((e) => e.sequence ?? e.id))
          : null);
      let hasMore = true;
      let pagesFetched = 0;
      const MAX_PAGES_PER_CATCHUP = 5;

      while (hasMore && pagesFetched < MAX_PAGES_PER_CATCHUP) {
        pagesFetched += 1;
        const res = await api.getManagementTaskEvents(scope, runId, cursor, 50);
        if (context.current !== expectedContext || eventVersions.current[runId] !== nextVer) return;

        if (res.items && res.items.length > 0) {
          const map = new Map(accumulated.map((e) => [e.sequence ?? e.id, e]));
          for (const item of res.items) {
            map.set(item.sequence ?? item.id, item);
          }
          accumulated = Array.from(map.values()).sort((a, b) => (a.sequence ?? a.id) - (b.sequence ?? b.id));
          const maxSeq = Math.max(...res.items.map((e) => e.sequence ?? e.id));
          cursor = Math.max(cursor ?? 0, maxSeq);
        }

        hasMore = res.hasMore;
        if (!res.hasMore || !res.items || res.items.length === 0) {
          break;
        }
      }

      if (context.current !== expectedContext || eventVersions.current[runId] !== nextVer) return;

      setRunEvents((prev) => ({ ...prev, [runId]: accumulated }));
      if (cursor !== null) {
        setEventCursors((prev) => ({ ...prev, [runId]: Math.max(prev[runId] ?? 0, cursor!) }));
      }
      setEventHasMore((prev) => ({ ...prev, [runId]: hasMore }));
    } catch (err) {
      if (context.current === expectedContext && eventVersions.current[runId] === nextVer) {
        setEventErrors((prev) => ({ ...prev, [runId]: `加载事件日志失败: ${api.asError(err)}` }));
      }
    } finally {
      if (context.current === expectedContext && eventVersions.current[runId] === nextVer) {
        setEventsLoading((prev) => ({ ...prev, [runId]: false }));
      }
    }
  };

  const toggleExpandRun = (runId: string) => {
    if (expandedRunId === runId) {
      setExpandedRunId(null);
    } else {
      setExpandedRunId(runId);
      void loadItemsForRun(runId);
      void loadEventsForRun(runId);
    }
  };

  const renderStatusBadge = (status: string) => {
    switch (status) {
      case "completed":
      case "confirmed":
      case "success":
        return (
          <Badge variant="outline" className="gap-1 border-emerald-500/40 text-emerald-600 dark:text-emerald-400">
            <CheckCircle2 className="h-3 w-3" /> 已完成
          </Badge>
        );
      case "already_claimed":
        return (
          <Badge variant="outline" className="gap-1 border-cyan-500/40 text-cyan-600 dark:text-cyan-400">
            <CheckCircle2 className="h-3 w-3" /> 此前已领
          </Badge>
        );
      case "waiting_verification":
      case "pending_confirmation":
        return (
          <Badge variant="outline" className="gap-1 border-amber-500/40 text-amber-600 dark:text-amber-400">
            <AlertCircle className="h-3 w-3" /> 待确认
          </Badge>
        );
      case "failed":
        return (
          <Badge variant="outline" className="gap-1 border-rose-500/40 text-rose-600 dark:text-rose-400">
            <XCircle className="h-3 w-3" /> 失败
          </Badge>
        );
      case "running":
      case "waiting_upstream":
        return (
          <Badge variant="outline" className="gap-1 border-blue-500/40 text-blue-600 dark:text-blue-400">
            <Loader2 className="h-3 w-3 animate-spin" /> 执行中
          </Badge>
        );
      case "cancelled":
        return (
          <Badge variant="outline" className="gap-1 border-neutral-500/40 text-neutral-600 dark:text-neutral-400">
            <XCircle className="h-3 w-3" /> 已取消
          </Badge>
        );
      case "skipped":
        return (
          <Badge variant="secondary" className="gap-1">
            跳过
          </Badge>
        );
      case "unsupported":
        return <Badge variant="secondary">未接入 · 未执行</Badge>;
      default:
        return (
          <Badge variant="secondary" className="gap-1">
            <Clock className="h-3 w-3" /> {status || "就绪"}
          </Badge>
        );
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-4xl max-h-[88vh] flex flex-col overflow-hidden">
        <DialogHeader className="shrink-0">
          <div className="flex items-center justify-between pr-6">
            <DialogTitle className="flex items-center gap-2 text-lg">
              <Play className="h-5 w-5 text-primary" />
              任务中心执行器与调度管理
            </DialogTitle>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                void loadData();
                if (expandedRunId) {
                  void loadItemsForRun(expandedRunId, true);
                  void loadEventsForRun(expandedRunId);
                }
              }}
              disabled={loading}
              className="gap-1.5"
            >
              <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
              刷新
            </Button>
          </div>
          <DialogDescription>
            作用域：<span className="font-mono font-medium text-foreground">{scope}</span>
            {" · "}
            查看任务规范、逐账号执行记录与待确认结果。
          </DialogDescription>
        </DialogHeader>

        <div className="shrink-0 my-2">
          <Alert variant="default" className="border-emerald-500/30 bg-emerald-500/5 text-emerald-900 dark:text-emerald-200">
            <Info className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
            <AlertTitle className="text-xs font-semibold">任务管理预览</AlertTitle>
            <AlertDescription className="text-xs mt-1 text-muted-foreground leading-relaxed">
              任务记录会保存在本机；停止操作取消未开始账号，在途操作保留为待确认，失败重试会跳过待确认和已领取账号。
              <span className="text-emerald-700 dark:text-emerald-300 font-medium ml-1">
                当前管理入口尚未接入真实平台动作，执行结果显示“未接入”，不代表签到或奖励到账。已启用计划会由应用内排程执行；中断的写操作保留待确认。
              </span>
            </AlertDescription>
          </Alert>
        </div>

        {error ? (
          <Alert variant="destructive" className="shrink-0 my-1">
            <XCircle className="h-4 w-4" />
            <AlertTitle className="text-xs font-semibold">执行异常</AlertTitle>
            <AlertDescription className="text-xs">{error}</AlertDescription>
          </Alert>
        ) : null}

        {actionSuccessMsg ? (
          <Alert variant="default" className="shrink-0 my-1 border-emerald-500/40 text-emerald-700 dark:text-emerald-300">
            <CheckCircle2 className="h-4 w-4" />
            <AlertDescription className="text-xs">{actionSuccessMsg}</AlertDescription>
          </Alert>
        ) : null}

        <div className="flex-1 overflow-y-auto pr-1 space-y-6">
          {/* 规范清单与手动触发 */}
          <section className="space-y-3">
            <h4 className="text-sm font-semibold flex items-center gap-1.5">
              <Calendar className="h-4 w-4 text-muted-foreground" />
              已登记任务规范清单 ({specs.length})
            </h4>

            {loading && specs.length === 0 ? (
              <div className="space-y-2">
                <Skeleton className="h-16 w-full rounded-md" />
                <Skeleton className="h-16 w-full rounded-md" />
              </div>
            ) : specs.length === 0 ? (
              <EmptyState
                icon={AlertCircle}
                title="暂无登记的任务规范"
                description={`当前作用域 (${scope}) 尚未登记自动化任务契约`}
              />
            ) : (
              <div className="grid gap-2.5">
                {specs.map((spec) => {
                  const isTriggering = actionLoading === `trigger-${spec.taskId}`;
                  return (
                    <div
                      key={spec.taskId}
                      className="p-3 rounded-lg border bg-card/60 hover:bg-card transition-colors flex flex-col gap-2.5"
                    >
                      <div className="flex items-start justify-between gap-2">
                        <div className="space-y-0.5">
                          <div className="flex items-center gap-2">
                            <span className="font-semibold text-sm">{spec.name}</span>
                            <span className="text-xs font-mono text-muted-foreground">({spec.taskId})</span>
                            {renderStatusBadge(spec.status)}
                          </div>
                          <p className="text-xs text-muted-foreground">{spec.description}</p>
                        </div>
                        <div className="flex items-center gap-2 shrink-0">
                          <Badge variant={spec.enabled ? "default" : "secondary"}>
                            {spec.enabled ? "计划已启用 · 平台动作未接入" : "计划已停用"}
                          </Badge>
                          <Button
                            size="sm"
                            variant="secondary"
                            className="gap-1.5 h-8 text-xs font-medium"
                            disabled={isTriggering || Boolean(actionLoading)}
                            onClick={() => void handleTriggerTask(spec.taskId)}
                          >
                            {isTriggering ? (
                              <Loader2 className="h-3 w-3 animate-spin" />
                            ) : (
                              <Play className="h-3 w-3 text-primary" />
                            )}
                            立即执行
                          </Button>
                        </div>
                      </div>
                      <div className="flex flex-wrap items-center gap-4 text-xs text-muted-foreground pt-1.5 border-t border-border/40 font-mono">
                        <span>触发类型: {spec.triggerType}</span>
                        <span>调度表达式: {spec.scheduleCron ?? spec.defaultSchedule}</span>
                        <span>时区: {spec.timezone ?? "未提供"}</span>
                        <span>最近执行: {spec.lastRunAt ? new Date(spec.lastRunAt * 1000).toLocaleString() : "尚未执行"}</span>
                        <span>下次计划: {spec.nextRunAt ? new Date(spec.nextRunAt * 1000).toLocaleString() : "未计划"}</span>
                        {typeof spec.metadata?.scheduleError === "string" ? <span className="text-rose-600">{spec.metadata.scheduleError}</span> : null}
                        {typeof spec.metadata?.accountCount === "number" ? (
                          <span className="flex items-center gap-1 text-foreground">
                            <Users className="h-3 w-3" />
                            关联账号: {String(spec.metadata.accountCount)} 个
                          </span>
                        ) : null}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </section>

          {/* 运行历史流水与逐账号详情 */}
          <section className="space-y-3">
            <h4 className="text-sm font-semibold flex items-center gap-1.5">
              <Clock className="h-4 w-4 text-muted-foreground" />
              执行记录与流水 ({runs.length})
            </h4>

            {runs.length === 0 ? (
              <div className="p-4 rounded-lg border border-dashed text-center text-xs text-muted-foreground">
                暂无运行记录
              </div>
            ) : (
              <div className="border rounded-lg overflow-hidden divide-y divide-border/60">
                {runs.map((run) => {
                  const isExpanded = expandedRunId === run.runId;
                  const isCancelling = actionLoading === `cancel-${run.runId}`;
                  const isRetrying = actionLoading === `retry-${run.runId}`;
                  const canCancel = matchesActive(run.status);
                  const canRetry = run.failedItems > 0 && !canCancel;
                  const currentItems = runItems[run.runId];
                  const currentEvents = runEvents[run.runId];
                  const isEventsLoading = Boolean(eventsLoading[run.runId]);

                  return (
                    <div key={run.runId} className="bg-card/40">
                      <div className="p-3 flex items-center justify-between gap-3 hover:bg-muted/30 transition-colors">
                        <button
                          type="button"
                          className="flex items-center gap-2.5 text-left flex-1 min-w-0"
                          onClick={() => toggleExpandRun(run.runId)}
                        >
                          {isExpanded ? (
                            <ChevronDown className="h-4 w-4 text-muted-foreground shrink-0" />
                          ) : (
                            <ChevronRight className="h-4 w-4 text-muted-foreground shrink-0" />
                          )}
                          <div className="space-y-0.5 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="font-mono font-medium text-xs truncate">{run.runId}</span>
                              {renderStatusBadge(run.status)}
                              <Badge variant="outline" className="text-[10px] font-mono">
                                {run.triggeredBy}
                              </Badge>
                            </div>
                            <div className="flex items-center gap-3 text-xs text-muted-foreground">
                              <span>任务: {run.taskId}</span>
                              <span>处理进度: {run.successItems} 成功 / {run.failedItems} 失败 / 共 {run.totalItems} 项</span>
                              {run.message ? (
                                <span className="truncate max-w-[240px] text-foreground/80 font-sans" title={run.message}>
                                  {run.message}
                                </span>
                              ) : null}
                            </div>
                          </div>
                        </button>

                        <div className="flex items-center gap-2 shrink-0">
                          {canCancel ? (
                            <Button
                              size="sm"
                              variant="outline"
                              className="h-7 text-xs border-rose-500/40 text-rose-600 dark:text-rose-400 hover:bg-rose-500/10 gap-1"
                              disabled={isCancelling}
                              onClick={() => void handleCancelTask(run.runId)}
                            >
                              {isCancelling ? <Loader2 className="h-3 w-3 animate-spin" /> : <Square className="h-3 w-3" />}
                              停止取消
                            </Button>
                          ) : null}

                          {canRetry ? (
                            <Button
                              size="sm"
                              variant="outline"
                              className="h-7 text-xs border-amber-500/40 text-amber-600 dark:text-amber-400 hover:bg-amber-500/10 gap-1"
                              disabled={isRetrying}
                              onClick={() => void handleRetryTask(run.runId)}
                            >
                              {isRetrying ? <Loader2 className="h-3 w-3 animate-spin" /> : <RotateCcw className="h-3 w-3" />}
                              重试失败项
                            </Button>
                          ) : null}
                        </div>
                      </div>

                      {/* 逐账号 item 详情展开 */}
                      {isExpanded ? (
                        <div className="p-3 bg-muted/20 border-t border-border/40 space-y-2">
                          <div className="text-xs font-semibold text-muted-foreground flex items-center justify-between">
                            <span>逐账号执行明细 (Items)</span>
                            {itemsLoading === run.runId ? (
                              <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                            ) : null}
                          </div>

                          {!currentItems || currentItems.length === 0 ? (
                            <div className="text-xs text-muted-foreground py-2 text-center">
                              {itemsLoading === run.runId ? "正在加载逐账号记录..." : "暂无逐账号详细记录"}
                            </div>
                          ) : (
                            <div className="border rounded divide-y divide-border/40 text-xs font-mono bg-card">
                              {currentItems.map((item) => (
                                <div key={item.accountId} className="p-2.5 flex items-center justify-between gap-3">
                                  <div className="space-y-0.5">
                                    <div className="flex items-center gap-2">
                                      <span className="font-semibold text-foreground">{item.accountId}</span>
                                      {renderStatusBadge(item.status)}
                                      <span className="text-muted-foreground text-[11px]">阶段: {item.stage}</span>
                                      {item.retryCount && item.retryCount > 0 ? (
                                        <Badge variant="outline" className="text-[10px] text-amber-600">
                                          已重试 {item.retryCount} 次
                                        </Badge>
                                      ) : null}
                                    </div>
                                    {item.error ? (
                                      <p className="text-[11px] text-rose-500 font-sans">{item.error}</p>
                                    ) : null}
                                  </div>
                                </div>
                              ))}
                            </div>
                          )}

                          {/* 增量持久化事件日志 (FUS-W06) */}
                          <div className="pt-2 border-t border-border/30 space-y-2">
                            <div className="text-xs font-semibold text-muted-foreground flex items-center justify-between">
                              <span className="flex items-center gap-1.5">
                                <ScrollText className="h-3.5 w-3.5 text-muted-foreground" />
                                生命周期事件日志 ({currentEvents?.length ?? 0} 条)
                              </span>
                              <div className="flex items-center gap-1.5">
                                {eventHasMore[run.runId] ? (
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    className="h-6 text-[11px] px-2 text-muted-foreground hover:text-foreground"
                                    disabled={isEventsLoading}
                                    onClick={() => void loadEventsForRun(run.runId)}
                                  >
                                    加载更多
                                  </Button>
                                ) : null}
                                <Button
                                  variant="ghost"
                                  size="sm"
                                  className="h-6 text-[11px] px-2 gap-1 text-muted-foreground hover:text-foreground"
                                  disabled={isEventsLoading}
                                  onClick={() => void loadEventsForRun(run.runId)}
                                >
                                  <RefreshCw className={`h-3 w-3 ${isEventsLoading ? "animate-spin" : ""}`} />
                                  增量补读
                                </Button>
                              </div>
                            </div>

                            {eventErrors[run.runId] ? (
                              <div className="text-xs text-rose-500 bg-rose-500/10 border border-rose-500/20 rounded p-2 flex items-center justify-between">
                                <span className="truncate max-w-[320px]">{eventErrors[run.runId]}</span>
                                <Button
                                  variant="ghost"
                                  size="sm"
                                  className="h-5 text-[11px] px-1 text-rose-600 hover:text-rose-700"
                                  onClick={() => void loadEventsForRun(run.runId)}
                                >
                                  重试
                                </Button>
                              </div>
                            ) : null}

                            {!currentEvents || currentEvents.length === 0 ? (
                              <div className="text-xs text-muted-foreground py-2 text-center">
                                {isEventsLoading ? "正在加载事件日志..." : "暂无持久化事件日志"}
                              </div>
                            ) : (
                              <div className="border rounded divide-y divide-border/40 text-xs font-mono bg-card max-h-48 overflow-y-auto">
                                {currentEvents.map((evt) => {
                                  const seqId = evt.sequence ?? evt.id;
                                  return (
                                    <div key={seqId} className="p-2 flex items-center justify-between gap-2 hover:bg-muted/30">
                                      <div className="flex items-center gap-2 min-w-0">
                                        <span className="text-[10px] text-muted-foreground font-semibold">#{seqId}</span>
                                        <span className="text-[11px] text-muted-foreground">
                                          {new Date(evt.occurredAtMs).toLocaleTimeString()}
                                        </span>
                                        <span className="font-semibold text-foreground truncate max-w-[120px]">
                                          {evt.accountId || "run"}
                                        </span>
                                        {renderStatusBadge(evt.status)}
                                        <Badge variant="outline" className="text-[10px]">
                                          {evt.stage}
                                        </Badge>
                                        {evt.errorCategory ? (
                                          <Badge variant="outline" className="text-[10px] border-rose-500/40 text-rose-600">
                                            {evt.errorCategory}
                                          </Badge>
                                        ) : null}
                                        {evt.message ? (
                                          <span className="text-[11px] text-muted-foreground truncate font-sans max-w-[280px]" title={evt.message}>
                                            {evt.message}
                                          </span>
                                        ) : null}
                                      </div>
                                    </div>
                                  );
                                })}
                              </div>
                            )}
                          </div>
                        </div>
                      ) : null}
                    </div>
                  );
                })}
              </div>
            )}
          </section>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function matchesActive(status: string): boolean {
  return ["queued", "running", "waiting_upstream", "waiting_verification"].includes(status);
}

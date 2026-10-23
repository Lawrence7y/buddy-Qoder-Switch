/**
 * 统一积分管理工作台（PRD 资产管理与日历视图）。
 *
 * ## 验收闭环标准：
 * 1. 真实来源积分汇总与状态条（live / cached / stale / unsupported / failed）；
 * 2. 4 格统计卡（总账号、总包数、7日内到期额度、剩余总额）；
 * 3. 可点击的到期日历（点击日期高亮并过滤明细表格，支持一键重置）；
 * 4. 积分包明细表格（账号、包类型、余额/总额进度、到期时间、状态）；
 * 5. 积分流水变动历史（增减标识、时间、原因）；
 * 6. 任务中心管理原型入口（契约弹窗，如实标注后台执行器后续阶段交付）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Calendar,
  CalendarClock,
  Coins,
  Database,
  History,
  Info,
  Package,
  Play,
  RefreshCw,
  X,
  XCircle,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { TaskCenterDialog } from "@/components/tasks/task-center-dialog";
import * as api from "@/lib/api";
import type { CreditSummaryProjection } from "@/lib/types";
import { todayKey, usageWindowBounds, usageWindowDays, type UsageWindow } from "@/lib/usage-window";

interface CreditManagementWorkbenchProps {
  scope: string;
  viewWindow?: UsageWindow;
  onRefreshParent?: () => void;
}

export function CreditManagementWorkbench({
  scope,
  viewWindow,
  onRefreshParent,
}: CreditManagementWorkbenchProps) {
  const [data, setData] = useState<CreditSummaryProjection | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 筛选状态
  const [selectedDate, setSelectedDate] = useState<string | null>(null);
  const [accountFilter, setAccountFilter] = useState<string>("all");
  const [typeFilter, setTypeFilter] = useState<string>("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [availableAccounts, setAvailableAccounts] = useState<string[]>([]);

  // 任务中心原型弹窗状态
  const [taskDialogOpen, setTaskDialogOpen] = useState(false);

  // 防竞态请求计数器
  const activeRequestRef = useRef(0);

  // 计算窗口天数跨度与自定义时间范围提示（自查项：防止静默错配）
  const { windowDays, customBoundsNotice } = useMemo(() => {
    if (!viewWindow) return { windowDays: undefined, customBoundsNotice: null };
    const days = usageWindowDays(viewWindow);
    if (viewWindow.kind === "custom") {
      const bounds = usageWindowBounds(viewWindow);
      const isPastCutoff = bounds.to !== todayKey();
      const notice = isPastCutoff
        ? `自定义时间范围 (${bounds.from} ~ ${bounds.to})：当前管理工作台按近 ${days} 天区间回溯聚合；历史截止范围已对齐近 ${days} 天窗口。`
        : null;
      return { windowDays: days, customBoundsNotice: notice };
    }
    return { windowDays: days, customBoundsNotice: null };
  }, [viewWindow]);

  // 加载数据（联动账号筛选与时间窗口）
  const loadProjection = useCallback(
    async (silent = false, currentAccount = accountFilter) => {
      const requestId = ++activeRequestRef.current;
      if (!silent) setLoading(true);
      setError(null);
      try {
        const filterAcc = currentAccount === "all" ? undefined : currentAccount;
        const projection = await api.getManagementCreditSummary(scope, filterAcc, windowDays);
        if (requestId !== activeRequestRef.current) {
          return; // 忽略过期响应
        }
        setData(projection);
        if (currentAccount === "all" && projection.packages) {
          const accs = new Set<string>();
          for (const pkg of projection.packages) {
            if (pkg.accountId) accs.add(pkg.accountId);
          }
          setAvailableAccounts(Array.from(accs));
        }
      } catch (err) {
        if (requestId === activeRequestRef.current) {
          setError(api.asError(err));
        }
      } finally {
        if (requestId === activeRequestRef.current && !silent) {
          setLoading(false);
        }
      }
    },
    [scope, accountFilter, windowDays]
  );

  // scope 或 windowDays 变更时重置筛选并全量拉取
  useEffect(() => {
    setAccountFilter("all");
    setSelectedDate(null);
    setTypeFilter("all");
    setSearchQuery("");
    void loadProjection(false, "all");
  }, [scope, windowDays]);

  // 账号筛选改变时触发带参加载
  const handleAccountChange = (val: string) => {
    setAccountFilter(val);
    void loadProjection(true, val);
  };

  // 主动刷新积分
  const handleRefresh = async () => {
    if (refreshing) return;
    setRefreshing(true);
    setError(null);
    try {
      const projection = await api.refreshManagementCredits(scope);
      setData(projection);
      if (onRefreshParent) {
        onRefreshParent();
      }
    } catch (err) {
      setError(api.asError(err));
    } finally {
      setRefreshing(false);
    }
  };

  // 安全格式化时间戳（防毫秒二次乘 1000 放大，无有效观测时返回未观测）
  const formatTime = (ts: number | null | undefined) => {
    if (!ts || ts <= 0) return "未观测";
    const ms = ts > 1e11 ? ts : ts * 1000;
    return new Date(ms).toLocaleString("zh-CN", {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  };

  // 提取包类型列表供筛选
  const typeOptions = useMemo(() => {
    if (!data?.packages) return [];
    const set = new Set<string>();
    for (const pkg of data.packages) {
      if (pkg.packageType) set.add(pkg.packageType);
    }
    return Array.from(set);
  }, [data?.packages]);

  // 过滤后的积分包列表
  const filteredPackages = useMemo(() => {
    if (!data?.packages) return [];
    return data.packages.filter((pkg) => {
      // 日历日期过滤
      if (selectedDate) {
        if (pkg.expireTimeFormatted !== selectedDate) return false;
      }
      // 账号过滤（后端已过滤，前端再次做兜底保护）
      if (accountFilter !== "all" && pkg.accountId !== accountFilter) {
        return false;
      }
      // 类型过滤
      if (typeFilter !== "all" && pkg.packageType !== typeFilter) {
        return false;
      }
      // 搜索关键词
      if (searchQuery.trim()) {
        const q = searchQuery.trim().toLowerCase();
        const nameMatch = pkg.packageName.toLowerCase().includes(q);
        const accMatch = pkg.accountId.toLowerCase().includes(q);
        const idMatch = pkg.packageId.toLowerCase().includes(q);
        if (!nameMatch && !accMatch && !idMatch) return false;
      }
      return true;
    });
  }, [data?.packages, selectedDate, accountFilter, typeFilter, searchQuery]);

  // 数据状态指示条渲染
  const renderStatusBanner = () => {
    if (error) {
      return (
        <Alert variant="destructive" className="border-rose-500/30 bg-rose-500/5">
          <XCircle className="h-4 w-4" />
          <AlertTitle className="text-xs font-semibold">积分数据加载失败</AlertTitle>
          <AlertDescription className="text-xs mt-1">
            {error}。请检查网络或后端状态，点击重试。
          </AlertDescription>
        </Alert>
      );
    }
    if (!data?.status) return null;
    const { code, message, observedAt, cachedAt, isStale } = data.status;

    if (code === "unsupported") {
      return (
        <Alert variant="default" className="border-amber-500/30 bg-amber-500/5">
          <Info className="h-4 w-4 text-amber-600 dark:text-amber-400" />
          <AlertTitle className="text-xs font-semibold">数据源能力状态：尚未提供逐包明细接口</AlertTitle>
          <AlertDescription className="text-xs text-muted-foreground mt-1">
            {message}。当前数据源具备官方额度查询能力，但尚未提供逐包资产拆分；按诚实规范不臆造虚假积分包。
          </AlertDescription>
        </Alert>
      );
    }

    if (code === "failed") {
      return (
        <Alert variant="destructive" className="border-rose-500/30 bg-rose-500/5">
          <XCircle className="h-4 w-4" />
          <AlertTitle className="text-xs font-semibold">积分数据同步异常</AlertTitle>
          <AlertDescription className="text-xs mt-1">
            {message}。未能成功读取或更新最新积分快照，您可以点击右侧按钮重试刷新。
          </AlertDescription>
        </Alert>
      );
    }

    if (code === "unobserved" || data.stats?.isKnown === false) {
      return (
        <div className="flex flex-wrap items-center justify-between gap-3 p-3 rounded-lg border bg-card/60 text-xs">
          <div className="flex items-center gap-3">
            <div className="flex items-center gap-1.5 font-medium">
              <Database className="h-4 w-4 text-muted-foreground" />
              <span>存储状态:</span>
              <Badge variant="outline" className="border-muted-foreground/40 text-muted-foreground">
                未观测 (Unobserved)
              </Badge>
            </div>
            <span className="text-muted-foreground">|</span>
            <span className="text-muted-foreground">
              最近观测：<span className="text-muted-foreground italic">未观测（无历史快照）</span>
            </span>
          </div>

          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => setTaskDialogOpen(true)}
              className="h-7 text-xs gap-1"
            >
              <Play className="h-3 w-3 text-primary" />
              任务中心原型
            </Button>
            <Button
              variant="default"
              size="sm"
              onClick={() => void handleRefresh()}
              disabled={refreshing}
              className="h-7 text-xs gap-1"
            >
              <RefreshCw className={`h-3 w-3 ${refreshing ? "animate-spin" : ""}`} />
              {refreshing ? "刷新中..." : "主动刷新积分"}
            </Button>
          </div>
        </div>
      );
    }

    return (
      <div className="flex flex-wrap items-center justify-between gap-3 p-3 rounded-lg border bg-card/60 text-xs">
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-1.5 font-medium">
            <Database className="h-4 w-4 text-primary" />
            <span>存储状态:</span>
            {code === "live" ? (
              <Badge variant="outline" className="border-emerald-500/40 text-emerald-600 dark:text-emerald-400">
                实时直连 (Live)
              </Badge>
            ) : isStale ? (
              <Badge variant="outline" className="border-amber-500/40 text-amber-600 dark:text-amber-400">
                缓存过期 (Stale)
              </Badge>
            ) : (
              <Badge variant="secondary">本地快照 (Cached)</Badge>
            )}
          </div>
          <span className="text-muted-foreground">|</span>
          {observedAt && observedAt > 0 ? (
            <span className="text-muted-foreground">
              最近观测时间：<span className="font-mono text-foreground">{formatTime(observedAt)}</span>
            </span>
          ) : (
            <span className="text-muted-foreground">
              最近观测：<span className="text-muted-foreground italic">未观测</span>
            </span>
          )}
          {cachedAt && cachedAt > 0 ? (
            <span className="text-muted-foreground hidden sm:inline">
              快照归档时间：<span className="font-mono text-foreground">{formatTime(cachedAt)}</span>
            </span>
          ) : null}
        </div>

        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setTaskDialogOpen(true)}
            className="h-7 text-xs gap-1"
          >
            <Play className="h-3 w-3 text-primary" />
            任务中心原型
          </Button>
          <Button
            variant="default"
            size="sm"
            onClick={() => void handleRefresh()}
            disabled={refreshing}
            className="h-7 text-xs gap-1"
          >
            <RefreshCw className={`h-3 w-3 ${refreshing ? "animate-spin" : ""}`} />
            {refreshing ? "刷新中..." : "主动刷新积分"}
          </Button>
        </div>
      </div>
    );
  };

  if (loading && !data) {
    return (
      <div className="space-y-4 my-4">
        <Skeleton className="h-12 w-full rounded-lg" />
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          <Skeleton className="h-20 w-full rounded-lg" />
          <Skeleton className="h-20 w-full rounded-lg" />
          <Skeleton className="h-20 w-full rounded-lg" />
          <Skeleton className="h-20 w-full rounded-lg" />
        </div>
        <Skeleton className="h-48 w-full rounded-lg" />
      </div>
    );
  }

  return (
    <div className="space-y-5 my-4">
      {/* 状态与控制栏 */}
      {renderStatusBanner()}

      {/* 自定义截止日期自查提示 */}
      {customBoundsNotice ? (
        <div className="text-[11px] text-muted-foreground flex items-center gap-1.5 px-3 py-1.5 rounded-md bg-muted/40 border border-border/50">
          <Info className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
          <span>{customBoundsNotice}</span>
        </div>
      ) : null}

      {/* 4 格统计卡 */}
      {data?.stats ? (
        (() => {
          const isKnown = data.stats.isKnown && data.status?.code !== "unobserved";
          return (
            <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
              <Card className="p-3.5 space-y-1">
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>覆盖账号数</span>
                  <Coins className="h-4 w-4 text-primary/70" />
                </div>
                <div className="text-xl font-bold font-mono">
                  {isKnown ? data.stats.totalAccounts : "—"}
                  {isKnown ? (
                    <span className="text-xs font-normal text-muted-foreground ml-1">个</span>
                  ) : null}
                </div>
                <p className="text-[11px] text-muted-foreground">按 (scope, id) 隔离</p>
              </Card>

              <Card className="p-3.5 space-y-1">
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>积分包总数</span>
                  <Package className="h-4 w-4 text-primary/70" />
                </div>
                <div className="text-xl font-bold font-mono">
                  {isKnown ? data.stats.totalPackages : "—"}
                  {isKnown ? (
                    <span className="text-xs font-normal text-muted-foreground ml-1">个包</span>
                  ) : null}
                </div>
                <p className="text-[11px] text-muted-foreground">含月度/赠送/体验包</p>
              </Card>

              <Card className="p-3.5 space-y-1">
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>7天内即将到期</span>
                  <CalendarClock className="h-4 w-4 text-amber-500" />
                </div>
                <div className="text-xl font-bold font-mono text-amber-600 dark:text-amber-400">
                  {isKnown ? (
                    <>
                      {data.stats.expiringWithin7DaysAmount.toLocaleString()}
                      <span className="text-xs font-normal text-muted-foreground ml-1">
                        ({data.stats.expiringWithin7DaysCount} 包)
                      </span>
                    </>
                  ) : (
                    "—"
                  )}
                </div>
                <p className="text-[11px] text-muted-foreground">需优先使用</p>
              </Card>

              <Card className="p-3.5 space-y-1">
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>当前可用余额</span>
                  <Database className="h-4 w-4 text-emerald-500" />
                </div>
                <div className="text-xl font-bold font-mono text-emerald-600 dark:text-emerald-400">
                  {isKnown ? data.stats.totalRemainingBalance.toLocaleString() : "—"}
                </div>
                <p className="text-[11px] text-muted-foreground">
                  总额度容量: {isKnown ? data.stats.totalOriginalCapacity.toLocaleString() : "未提供"}
                </p>
              </Card>
            </div>
          );
        })()
      ) : null}

      {/* 可点击到期日历 (Expiry Calendar) */}
      <Card className="p-4 space-y-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Calendar className="h-4 w-4 text-primary" />
            <h3 className="text-sm font-semibold">积分到期日历与倒计时</h3>
            <span className="text-xs text-muted-foreground">
              (点击日期卡片可联动过滤下方积分包明细)
            </span>
          </div>

          {selectedDate ? (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setSelectedDate(null)}
              className="h-7 text-xs text-muted-foreground hover:text-foreground gap-1"
            >
              <X className="h-3.5 w-3.5" />
              清除日期筛选 ({selectedDate})
            </Button>
          ) : null}
        </div>

        {data?.calendar && data.calendar.length > 0 ? (
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6 gap-2.5">
            {data.calendar.map((group) => {
              const isSelected = selectedDate === group.date;
              const isImminent = group.daysRemaining <= 7;
              return (
                <button
                  key={group.date}
                  type="button"
                  onClick={() =>
                    setSelectedDate(isSelected ? null : group.date)
                  }
                  className={`p-2.5 rounded-lg border text-left transition-all ${
                    isSelected
                      ? "border-primary bg-primary/10 shadow-sm"
                      : "border-border/70 hover:border-border hover:bg-muted/40"
                  }`}
                >
                  <div className="flex items-center justify-between text-[11px]">
                    <span className="font-mono font-medium">{group.date}</span>
                    <Badge
                      variant={isImminent ? "destructive" : "secondary"}
                      className="text-[10px] px-1 py-0 h-4"
                    >
                      {group.daysRemaining === 0
                        ? "今日到期"
                        : `${group.daysRemaining}天后`}
                    </Badge>
                  </div>
                  <div className="mt-2 flex items-baseline justify-between">
                    <span className="text-sm font-bold font-mono text-foreground">
                      {group.totalExpiring.toLocaleString()}
                    </span>
                    <span className="text-[10px] text-muted-foreground">
                      {group.packageCount} 个包
                    </span>
                  </div>
                </button>
              );
            })}
          </div>
        ) : (
          <div className="p-4 rounded-lg border border-dashed text-center text-xs text-muted-foreground">
            近期暂无明确到期的积分包记录
          </div>
        )}
      </Card>

      {/* 积分包明细表格与一致筛选器 */}
      <Card className="p-4 space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <Package className="h-4 w-4 text-primary" />
            <h3 className="text-sm font-semibold">积分包资产明细</h3>
            <span className="text-xs text-muted-foreground">
              (共 {filteredPackages.length} / {data?.packages?.length ?? 0} 项)
            </span>
          </div>

          {/* 筛选器工具栏 */}
          <div className="flex flex-wrap items-center gap-2 text-xs">
            {availableAccounts.length > 1 ? (
              <Select value={accountFilter} onValueChange={handleAccountChange}>
                <SelectTrigger className="h-8 w-36 text-xs">
                  <SelectValue placeholder="筛选账号" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">全部账号</SelectItem>
                  {availableAccounts.map((acc) => (
                    <SelectItem key={acc} value={acc}>
                      {acc}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : null}

            {typeOptions.length > 1 ? (
              <Select value={typeFilter} onValueChange={setTypeFilter}>
                <SelectTrigger className="h-8 w-32 text-xs">
                  <SelectValue placeholder="包类型" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">全部类型</SelectItem>
                  {typeOptions.map((tp) => (
                    <SelectItem key={tp} value={tp}>
                      {tp}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : null}

            <Input
              placeholder="搜索包名/账号/ID..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="h-8 w-44 text-xs"
            />
          </div>
        </div>

        {filteredPackages.length === 0 ? (
          <div className="p-8 text-center border border-dashed rounded-lg space-y-2">
            <p className="text-xs text-muted-foreground">没有符合当前筛选条件的积分包</p>
            {(selectedDate || accountFilter !== "all" || typeFilter !== "all" || searchQuery) ? (
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setSelectedDate(null);
                  handleAccountChange("all");
                  setTypeFilter("all");
                  setSearchQuery("");
                }}
                className="text-xs"
              >
                重置全部筛选
              </Button>
            ) : null}
          </div>
        ) : (
          <div className="border rounded-lg overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="bg-muted/50 border-b">
                <tr>
                  <th className="px-3 py-2 text-left font-medium">账号</th>
                  <th className="px-3 py-2 text-left font-medium">积分包名称</th>
                  <th className="px-3 py-2 text-left font-medium">类型</th>
                  <th className="px-3 py-2 text-left font-medium">可用余额 / 原始额度</th>
                  <th className="px-3 py-2 text-left font-medium">到期时间</th>
                  <th className="px-3 py-2 text-left font-medium">最近观测</th>
                  <th className="px-3 py-2 text-left font-medium">状态</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border/60">
                {filteredPackages.map((pkg) => {
                  const capacity = pkg.originalCapacity ?? pkg.remainingBalance;
                  const ratio = capacity > 0 ? Math.min(100, Math.max(0, (pkg.remainingBalance / capacity) * 100)) : 0;
                  return (
                    <tr key={`${pkg.accountId}-${pkg.packageId}`} className="hover:bg-muted/30">
                      <td className="px-3 py-2 font-mono font-medium">{pkg.accountId}</td>
                      <td className="px-3 py-2">
                        <div className="font-medium text-foreground">{pkg.packageName}</div>
                        <div className="font-mono text-[10px] text-muted-foreground">{pkg.packageId}</div>
                      </td>
                      <td className="px-3 py-2">
                        <Badge variant="outline" className="text-[10px] font-normal">
                          {pkg.packageType}
                        </Badge>
                      </td>
                      <td className="px-3 py-2 min-w-[160px]">
                        <div className="flex items-center justify-between font-mono text-[11px] mb-1">
                          <span className="font-semibold text-foreground">
                            {pkg.remainingBalance.toLocaleString()}
                          </span>
                          <span className="text-muted-foreground">
                            / {pkg.originalCapacity ? pkg.originalCapacity.toLocaleString() : "未提供"} {pkg.unit}
                          </span>
                        </div>
                        <div className="h-1.5 w-full bg-muted rounded-full overflow-hidden">
                          <div
                            className="h-full bg-primary rounded-full transition-all"
                            style={{ width: `${ratio}%` }}
                          />
                        </div>
                      </td>
                      <td className="px-3 py-2 font-mono">
                        {pkg.expireTimeFormatted ? (
                          <span className="text-foreground">{pkg.expireTimeFormatted}</span>
                        ) : (
                          <span className="text-muted-foreground italic">未提供到期日</span>
                        )}
                      </td>
                      <td className="px-3 py-2 font-mono text-muted-foreground">
                        {formatTime(pkg.observedAt)}
                      </td>
                      <td className="px-3 py-2">
                        <Badge
                          variant={pkg.status === "active" ? "secondary" : "outline"}
                          className="text-[10px]"
                        >
                          {pkg.status === "active" ? "有效" : pkg.status}
                        </Badge>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {/* 积分流水变动历史 */}
      {data?.recentEvents && data.recentEvents.length > 0 ? (
        <Card className="p-4 space-y-3">
          <div className="flex items-center gap-2">
            <History className="h-4 w-4 text-primary" />
            <h3 className="text-sm font-semibold">积分变更流水历史</h3>
          </div>

          <div className="border rounded-lg overflow-hidden">
            <table className="w-full text-xs">
              <thead className="bg-muted/50 border-b">
                <tr>
                  <th className="px-3 py-2 text-left font-medium">时间</th>
                  <th className="px-3 py-2 text-left font-medium">账号</th>
                  <th className="px-3 py-2 text-left font-medium">变动类型</th>
                  <th className="px-3 py-2 text-left font-medium">变动额度</th>
                  <th className="px-3 py-2 text-left font-medium">变动后余额</th>
                  <th className="px-3 py-2 text-left font-medium">说明</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border/60 font-mono">
                {data.recentEvents.map((evt) => (
                  <tr key={evt.eventId} className="hover:bg-muted/30">
                    <td className="px-3 py-2 text-muted-foreground">{formatTime(evt.recordedAt)}</td>
                    <td className="px-3 py-2">{evt.accountId}</td>
                    <td className="px-3 py-2 font-sans">{evt.eventType}</td>
                    <td className="px-3 py-2 font-semibold">
                      {evt.changeAmount > 0 ? (
                        <span className="text-emerald-600 dark:text-emerald-400">+{evt.changeAmount}</span>
                      ) : evt.changeAmount < 0 ? (
                        <span className="text-rose-600 dark:text-rose-400">{evt.changeAmount}</span>
                      ) : (
                        <span className="text-muted-foreground">0</span>
                      )}
                    </td>
                    <td className="px-3 py-2">{evt.balanceAfter.toLocaleString()}</td>
                    <td className="px-3 py-2 font-sans text-muted-foreground">{evt.reason}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Card>
      ) : null}

      {/* 任务中心弹窗 */}
      <TaskCenterDialog
        open={taskDialogOpen}
        onOpenChange={setTaskDialogOpen}
        scope={scope}
      />
    </div>
  );
}

/* NightDisplay 4.0 —— 界面脚本。
 *
 * 分工：后端 (Rust) 已经把"要显示什么"算好了（snapshot），这里只做渲染和发命令。
 * 所有判断（该不该黑、能不能启用、哪一步失败）都在后端，前端不重复实现，
 * 避免界面说的和实际做的不一致。
 *
 * 没有打包器：直接用 window.__TAURI__ 的全局 API（tauri.conf.json 里
 * withGlobalTauri = true）。
 */
'use strict';

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const appWindow = window.__TAURI__.window.getCurrentWindow();

/* ------------------------------------------------------------------ */
/* 小工具                                                              */
/* ------------------------------------------------------------------ */

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text != null) node.textContent = text;
  return node;
};

function esc(text) {
  return String(text == null ? '' : text);
}

function pad(n) {
  return String(n).padStart(2, '0');
}

/** 秒数 → 1:23:45 / 12:34 */
function hms(totalSeconds) {
  const s = Math.max(0, Math.floor(totalSeconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

function bytes(n) {
  if (!n) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i += 1; }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

const STATUS_TEXT = {
  ok: '已完成',
  skipped: '已跳过',
  manual: '需要手动处理',
  failed: '失败',
  pending: '等待执行',
};

const STEP_TITLES = {
  elevate: '授权',
  virtual_driver: '虚拟屏',
  autostart: '自启',
  input_guard: '防触控',
  self_test: '自检',
  confirm: '确认远程能连',
};

/* ------------------------------------------------------------------ */
/* Toast                                                               */
/* ------------------------------------------------------------------ */

function toast(message, kind) {
  if (!message) return;
  const node = el('div', `toast ${kind || ''}`, message);
  $('toasts').appendChild(node);
  setTimeout(() => {
    node.classList.add('out');
    setTimeout(() => node.remove(), 240);
  }, kind === 'bad' ? 7000 : 4200);
}

/* ------------------------------------------------------------------ */
/* 全局状态                                                            */
/* ------------------------------------------------------------------ */

const store = {
  snap: null,
  page: 'home',
  logFiles: [],
  logIndex: null,
  currentLog: null,
  countdownTimer: null,
  busy: false,
};

/* ------------------------------------------------------------------ */
/* 导航                                                                */
/* ------------------------------------------------------------------ */

function showPage(name) {
  store.page = name;
  document.querySelectorAll('.page').forEach((section) => {
    section.hidden = section.dataset.page !== name;
  });
  document.querySelectorAll('.nav-item').forEach((item) => {
    item.classList.toggle('active', item.dataset.target === name);
  });
  if (name === 'logs') {
    refreshLogs();
  }
  if (name === 'settings' || name === 'mode' || name === 'schedule' || name === 'about') {
    render();
  }
}

document.querySelectorAll('.nav-item').forEach((item) => {
  item.addEventListener('click', () => showPage(item.dataset.target));
});

/* 托盘菜单里的"打开设置"会发这个事件。 */
listen('navigate', (event) => {
  if (event.payload) showPage(event.payload);
});

/* 托盘动作的结果。 */
listen('toast', (event) => {
  toast(String(event.payload || ''), 'ok');
});

window.addEventListener('hashchange', () => {
  const target = location.hash.replace('#', '');
  if (target) showPage(target);
});

/* ------------------------------------------------------------------ */
/* 窗口按钮                                                            */
/* ------------------------------------------------------------------ */

$('win-min').addEventListener('click', () => appWindow.minimize());
$('win-max').addEventListener('click', async () => {
  if (await appWindow.isMaximized()) await appWindow.unmaximize();
  else await appWindow.maximize();
});
$('win-close').addEventListener('click', () => appWindow.close());

/* ------------------------------------------------------------------ */
/* 渲染：首页                                                          */
/* ------------------------------------------------------------------ */

function kvRow(label, value, cls) {
  const row = el('div', 'kv-row');
  row.appendChild(el('span', null, label));
  const b = el('b', cls || null, value);
  row.appendChild(b);
  return row;
}

function chip(text, state) {
  const node = el('span', 'chip');
  if (state) node.appendChild(el('i', `dot ${state}`));
  node.appendChild(el('span', null, text));
  return node;
}

/* 环的半径写在 index.html 里（r=94），周长 2πr ≈ 590.6，和 style.css 的
   stroke-dasharray 必须是同一个数，所以只在这里算一次。 */
const RING_RADIUS = 94;
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

function ringOffset(progress) {
  const p = Math.min(1, Math.max(0, Number(progress) || 0));
  return String(RING_CIRCUMFERENCE * (1 - p));
}

/* 把算出来的周长写回 CSS，避免两处手工填的 590.6 哪天改半径后对不上。 */
function primeRing() {
  const ring = $('ring-value');
  if (!ring) return;
  ring.style.strokeDasharray = String(RING_CIRCUMFERENCE);
  ring.style.strokeDashoffset = String(RING_CIRCUMFERENCE);
}

function renderRing(snap) {
  const ring = $('ring-value');
  const c = snap.countdown;
  ring.style.strokeDashoffset = ringOffset(c.progress);
  ring.classList.toggle('night', !!c.night);

  $('ring-state').textContent = snap.state === 'night' ? '夜间 · 屏幕已黑' : '白天 · 正常显示';
  $('ring-time').textContent = c.seconds > 0 ? hms(c.seconds) : '—';
  $('ring-hint').textContent = c.text || '';

  const modeLabel = snap.night_mode
    ? `${modeName(snap.night_mode)}`
    : `${modeName(snap.effective_mode)}（待命）`;
  $('legend-mode').textContent = modeLabel;
  const daemonDot = $('legend-daemon-dot');
  daemonDot.className = `dot ${snap.daemon_running ? 'ok' : 'warn'}`;
  $('legend-daemon').textContent = snap.daemon_running ? '运行中' : '未运行';

  const tag = $('brand-state');
  tag.textContent = snap.state === 'night' ? '夜间' : snap.enabled ? '计划已启用' : '未启用';
  tag.classList.toggle('night', snap.state === 'night');
}

function renderHome(snap) {
  renderRing(snap);

  $('home-sub').textContent = snap.enabled
    ? `${snap.ranges.map((r) => `${r.start} → ${r.end}`).join('，')}｜${snap.test_note}`
    : '每日计划未启用。先点「测试 15 秒」，确认远程能看到画面，再启用。';

  const kv = $('home-kv');
  kv.replaceChildren();
  kv.appendChild(kvRow('状态', snap.state === 'night' ? '夜间（本地已黑）' : '白天'));
  kv.appendChild(kvRow('模式', `${modeName(snap.effective_mode)}`));
  kv.appendChild(kvRow('后台服务', snap.daemon_running ? '运行中' : '未运行（点击动作会自动拉起）'));
  kv.appendChild(kvRow('解锁组合键', `${snap.unlock_chord}（按住 ${snap.unlock_hold_ms} 毫秒）`));
  kv.appendChild(kvRow('上次进入夜间耗时', snap.last_night_latency_ms ? `${snap.last_night_latency_ms} 毫秒` : '—'));

  const chips = $('home-chips');
  chips.replaceChildren();
  chips.appendChild(chip(`Windows ${snap.os || '未知'}`, snap.build >= 19041 || snap.build === 0 ? 'ok' : 'warn'));
  chips.appendChild(chip(`显示器 ${snap.displays} 块`, snap.displays > 0 ? 'ok' : 'bad'));
  chips.appendChild(chip(snap.virtual_display ? '虚拟屏已就绪' : '未安装虚拟屏', snap.virtual_display ? 'ok' : 'warn'));
  chips.appendChild(chip(snap.capture_exclusion ? '黑幕不进采集' : '不支持采集排除', snap.capture_exclusion ? 'ok' : 'bad'));
  chips.appendChild(chip(`${snap.brightness_displays} 块屏支持亮度调节`, snap.brightness_displays > 0 ? 'ok' : 'warn'));
  snap.gpu.forEach((gpu) => chips.appendChild(chip(gpu, null)));

  const list = $('home-monitors');
  list.replaceChildren();
  snap.monitors.forEach((line) => {
    const li = el('li');
    li.textContent = line;
    list.appendChild(li);
  });
  if (snap.monitors.length === 0) {
    const li = el('li', null, '读不到显示器明细（不影响使用，可在诊断包里看）。');
    list.appendChild(li);
  }

  // 需要处理：没装驱动 / 向导没走完。
  const wizard = snap.onboarding;
  const needsOnboarding = !wizard.completed;
  const needsDriver = !snap.virtual_display;
  const warn = $('home-warning');
  if (needsOnboarding || needsDriver) {
    warn.hidden = false;
    const reasons = [];
    if (needsOnboarding) {
      reasons.push(wizard.failed > 0
        ? `安装向导还有 ${wizard.failed} 个步骤需要处理。`
        : '安装向导还没有走完 —— 走完之后夜间切换才不会弹管理员确认。');
    }
    if (needsDriver) {
      reasons.push('虚拟显示驱动还没装好。「虚拟屏」模式需要它，装好后随时可用（不影响现在的桌面布局）。');
    }
    $('home-warning-text').textContent = reasons.join(' ');
  } else {
    warn.hidden = true;
  }

  $('side-summary').textContent = snap.state === 'night'
    ? '夜间模式进行中'
    : snap.enabled ? '等待今晚的时间点' : '每日计划未启用';
  $('side-daemon').textContent = snap.daemon_running ? '后台服务运行中' : '后台服务未运行';
  $('nav-log-dot').hidden = wizard.failed === 0;
}

/* ------------------------------------------------------------------ */
/* 渲染：定时计划                                                      */
/* ------------------------------------------------------------------ */

function renderSchedule(snap) {
  const range = snap.ranges[0] || { start: '20:00', end: '10:30', length: '—', overnight: false };
  if (document.activeElement !== $('in-start')) $('in-start').value = range.start;
  if (document.activeElement !== $('in-end')) $('in-end').value = range.end;
  $('tag-range-len').textContent = range.length;
  $('range-hint').textContent = range.overnight
    ? `跨过午夜：${range.start} 黑屏，第二天 ${range.end} 恢复，共 ${range.length}。`
    : `${range.start} 黑屏，${range.end} 当天恢复，共 ${range.length}。`;

  setSwitch('sw-enabled', snap.enabled);
  $('sw-enabled').disabled = !snap.enabled && !snap.test_ready;

  const notice = $('enable-notice');
  if (!snap.test_ready && !snap.enabled) {
    notice.hidden = false;
    notice.className = 'notice';
    notice.textContent = `还不能启用：${snap.test_note}。点右上角「测试 15 秒」，等屏幕自己亮回来，再打开这个开关。`;
  } else if (snap.enabled) {
    notice.hidden = false;
    notice.className = 'notice';
    notice.textContent = `每日计划已启用，系统里登记了 ${snap.registration.schedule_tasks} 个时间点任务。`;
  } else {
    notice.hidden = true;
  }

  const kv = $('schedule-kv');
  kv.replaceChildren();
  kv.appendChild(kvRow('上次测试', snap.test_note));
  kv.appendChild(kvRow('时间点任务', `${snap.registration.schedule_tasks} 个`));
  kv.appendChild(kvRow('登录自启任务', snap.registration.task_logon ? '已登记' : '未登记'));
  kv.appendChild(kvRow('开机自启任务', snap.registration.task_boot ? '已登记' : '未登记'));
  kv.appendChild(kvRow('看门狗任务', snap.registration.watchdog_task ? '已登记' : '未登记'));
}

/* ------------------------------------------------------------------ */
/* 渲染：显示模式                                                      */
/* ------------------------------------------------------------------ */

function modeName(mode) {
  return {
    auto: '自动（优先虚拟屏）',
    virtual_screen: '虚拟屏',
    overlay: '黑幕覆盖',
    screen_off: '关闭屏幕',
  }[mode] || mode;
}

const MODES = [
  {
    id: 'auto',
    name: '自动（推荐）',
    desc: '优先用虚拟屏；这台机器切不过去时自动降级成黑幕覆盖，并在日志里写明原因。',
    flag: '不用选择困难',
  },
  {
    id: 'virtual_screen',
    name: '虚拟屏',
    desc: '把桌面交给虚拟显示器，实体屏保持点亮但不带桌面。远程画面不中断，本地屏幕看不到内容。',
    flag: '需要内置驱动',
  },
  {
    id: 'overlay',
    name: '黑幕覆盖',
    desc: '在每块屏幕上盖一层不参与屏幕采集的黑色窗口。依赖 Windows 10 2004 及以上。',
    flag: '兼容性最好',
  },
  {
    id: 'screen_off',
    name: '关闭屏幕',
    desc: '直接发显示器待机命令。最省电，但远程软件通常会跟着失去画面。',
    flag: '远程会看到黑屏',
  },
];

function renderMode(snap) {
  const grid = $('mode-grid');
  grid.replaceChildren();
  MODES.forEach((mode) => {
    const card = el('button', `mode-card${snap.mode === mode.id ? ' active' : ''}`);
    card.type = 'button';
    const check = el('span', 'mode-check');
    check.innerHTML = '<svg viewBox="0 0 12 12"><path d="M2 6.4 4.6 9 10 3.4"/></svg>';
    card.appendChild(check);
    const h = el('h3');
    h.appendChild(document.createTextNode(mode.name));
    if (snap.effective_mode === mode.id && snap.mode !== mode.id) {
      h.appendChild(el('span', 'tag accent', '当前生效'));
    }
    card.appendChild(h);
    card.appendChild(el('p', null, mode.desc));
    card.appendChild(el('span', 'mode-flag', mode.flag));
    card.addEventListener('click', () => saveSettings({ mode: mode.id }));
    grid.appendChild(card);
  });

  const kv = $('mode-kv');
  kv.replaceChildren();
  kv.appendChild(kvRow('Windows', `${snap.os}（build ${snap.build}）`));
  kv.appendChild(kvRow('显示器', `${snap.displays} 块`));
  kv.appendChild(kvRow('虚拟屏', snap.virtual_display ? '驱动已就绪' : '未安装'));
  kv.appendChild(kvRow('虚拟屏正在接管桌面', snap.virtual_screen_holding ? '是' : '否'));
  kv.appendChild(kvRow('黑幕排除采集', snap.capture_exclusion ? '支持（WDA_EXCLUDEFROMCAPTURE）' : '不支持'));
  kv.appendChild(kvRow('Gamma 控制', snap.gamma_control ? '可用' : '不可用'));
  kv.appendChild(kvRow('DDC 亮度', `${snap.brightness_displays} 块屏`));
}

/* ------------------------------------------------------------------ */
/* 渲染：日志                                                          */
/* ------------------------------------------------------------------ */

/* 夜间会话里每一步的中文名。和 nd-core 的 session 模块一一对应。 */
const SESSION_STEPS = {
  prewarm: '预热虚拟屏（提前枚举、对齐模式）',
  topology: '切换显示路径（单次 SetDisplayConfig）',
  mode_match: '模式匹配（请求 → 实际）',
  settle: '等待显示稳定',
  active_paths: '检查活动输出数量',
  overlay: '黑幕显示与采集排除',
  gamma: '应用 gamma（压黑）',
  brightness: 'DDC 亮度',
  keep_awake: '阻止睡眠',
  input_hook: '本地键鼠钩子',
  lock_suppression: '抑制锁屏',
  restore: '恢复显示',
};

function stepNode(step) {
  // StepRecord 的三种形态：成功（ok=true，无说明）、跳过（ok=true，说明以"跳过："开头）、
  // 失败（ok=false，说明是原因）。跳过必须和成功区分开，否则用户会以为那一步真的做了。
  const skipped = step.ok && String(step.detail || '').startsWith('跳过');
  const status = !step.ok ? 'failed' : skipped ? 'skipped' : 'ok';
  const mark = status === 'ok' ? '✓' : status === 'failed' ? '✕' : '–';
  const node = el('div', 'step');
  node.appendChild(el('span', `step-mark ${status}`, mark));
  const name = el('span', 'step-name', SESSION_STEPS[step.step] || step.step);
  if (step.detail) name.appendChild(el('span', 'step-detail', step.detail));
  node.appendChild(name);
  node.appendChild(el('span', 'step-ms', step.ms ? `${step.ms} ms` : ''));
  return node;
}

function sessionNode(session, live) {
  const card = el('article', 'card session-card');
  const head = el('div', 'session-head');
  const title = el('div', 'session-title');
  title.appendChild(el('i', `dot ${session.failures > 0 ? 'bad' : session.ended_cleanly ? 'ok' : 'warn'}`));
  title.appendChild(el('span', null, `${session.started_at} · ${session.mode}`));
  head.appendChild(title);
  const meta = el('div', 'session-meta');
  meta.appendChild(el('span', null, session.ended_at ? `到 ${session.ended_at}` : '进行中'));
  if (session.duration_secs) meta.appendChild(el('span', null, `共 ${Math.round(session.duration_secs / 60)} 分钟`));
  if (session.failures > 0) meta.appendChild(el('span', null, `${session.failures} 步失败`));
  head.appendChild(meta);
  card.appendChild(head);

  if (session.summary) {
    card.appendChild(el('p', 'hint', session.summary));
  }

  const steps = el('div', 'steps');
  (session.steps || []).forEach((step) => steps.appendChild(stepNode(step)));
  if (!session.steps || session.steps.length === 0) {
    steps.appendChild(el('p', 'hint', live ? '正在执行…' : '这次会话没有记录步骤。'));
  }
  card.appendChild(steps);
  return card;
}

function renderSessions(snap) {
  const current = $('current-session-card');
  if (snap.current_session) {
    current.hidden = false;
    $('current-session-trace').textContent = snap.current_session.trace_id;
    const steps = $('current-session-steps');
    steps.replaceChildren();
    (snap.current_session.steps || []).forEach((step) => steps.appendChild(stepNode(step)));
  } else {
    current.hidden = true;
  }

  const list = $('session-list');
  list.replaceChildren();
  const sessions = snap.sessions || [];
  sessions.forEach((session) => list.appendChild(sessionNode(session, false)));
  $('session-empty').hidden = sessions.length > 0 || !!snap.current_session;
}

async function refreshLogs() {
  try {
    const index = await invoke('log_index');
    store.logIndex = index;
    renderLogFiles(index);
    if (!store.currentLog) {
      const daemon = index.files.find((f) => f.name.startsWith('daemon-') && f.name.endsWith('.log'));
      if (daemon) openLog(daemon);
      else if (index.files.length > 0) openLog(index.files[0]);
    }
  } catch (e) {
    toast(`读不到日志目录：${e}`, 'bad');
  }
}

function renderLogFiles(index) {
  const box = $('log-files');
  box.replaceChildren();
  if (index.files.length === 0) {
    box.appendChild(el('p', 'hint', '还没有日志文件。'));
    return;
  }
  const groups = new Map();
  index.files.forEach((file) => {
    const key = file.name.split('-')[0];
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(file);
  });
  const labels = {};
  index.components.forEach((c) => { labels[c.id] = c.label; });
  groups.forEach((files, key) => {
    box.appendChild(el('div', 'log-group', labels[key] || key));
    files.slice(0, 12).forEach((file) => {
      const item = el('button', `log-item${store.currentLog === file.path ? ' active' : ''}`);
      item.type = 'button';
      const label = el('span', null, file.name.replace(`${key}-`, ''));
      item.appendChild(label);
      item.appendChild(el('i', null, bytes(file.bytes)));
      item.addEventListener('click', () => openLog(file));
      box.appendChild(item);
    });
  });
}

async function openLog(file) {
  try {
    const tail = await invoke('read_logs', { query: { file: file.name, lines: 600 } });
    store.currentLog = file.path;
    $('log-title').textContent = file.name;
    $('log-meta').textContent = `${bytes(tail.bytes)} · 最后修改 ${tail.updated} · 显示最后若干行`;
    const body = $('log-body');
    body.textContent = tail.text || '（这个文件现在是空的）';
    body.scrollTop = body.scrollHeight;
    document.querySelectorAll('.log-item').forEach((node) => {
      node.classList.toggle('active', node.querySelector('span').textContent === file.name.replace(`${file.name.split('-')[0]}-`, ''));
    });
  } catch (e) {
    toast(`读不到这个日志：${e}`, 'bad');
  }
}

/* ------------------------------------------------------------------ */
/* 渲染：设置                                                          */
/* ------------------------------------------------------------------ */

function setSwitch(id, on) {
  const node = $(id);
  if (node) node.setAttribute('aria-checked', on ? 'true' : 'false');
}

function isSwitchOn(id) {
  return $(id).getAttribute('aria-checked') === 'true';
}

function renderSettings(snap) {
  setSwitch('sw-autostart', snap.autostart_preference);
  setSwitch('sw-strict', snap.strict_local_input);
  setSwitch('sw-lock', snap.suppress_lock);
  setSwitch('sw-awake', snap.keep_awake);
  setSwitch('sw-perf', snap.high_performance_at_night);

  if (document.activeElement !== $('in-chord')) $('in-chord').value = snap.unlock_chord;
  if (document.activeElement !== $('in-hold')) $('in-hold').value = String(snap.unlock_hold_ms);

  const reg = snap.registration;
  const kv = $('autostart-kv');
  kv.replaceChildren();
  kv.appendChild(kvRow('Run 键（登录）', reg.run_key ? '已注册' : '未注册'));
  kv.appendChild(kvRow('登录计划任务', reg.task_logon ? '已登记' : '未登记'));
  kv.appendChild(kvRow('开机计划任务（SYSTEM）', reg.task_boot ? '已登记' : '未登记'));
  kv.appendChild(kvRow('看门狗', reg.watchdog_task ? '已登记' : '未登记'));
  if (reg.run_key !== snap.autostart_preference) {
    kv.appendChild(kvRow('注意', '配置与实际注册状态不一致，点「重新授权 / 修复」', 'tag bad'));
  }

  const paths = $('paths-kv');
  paths.replaceChildren();
  paths.appendChild(kvRow('数据目录', snap.home));
  paths.appendChild(kvRow('日志目录', snap.logs_dir));
}

/* ------------------------------------------------------------------ */
/* 渲染：关于                                                          */
/* ------------------------------------------------------------------ */

function renderAbout(snap) {
  $('about-version').textContent = `v${snap.version}`;
  $('about-text').textContent =
    `NightDisplay ${snap.version} —— 到了设定的时间，本机屏幕变黑，远程软件照常能看到并操作桌面。` +
    '所有显示相关的动作都在后台服务里完成，关掉这个窗口不影响今晚的黑屏。';
  $('about-status').textContent = snap.status_text || '—';
}

/* ------------------------------------------------------------------ */
/* 总渲染                                                              */
/* ------------------------------------------------------------------ */

function render() {
  const snap = store.snap;
  if (!snap) return;
  renderHome(snap);
  renderSchedule(snap);
  renderMode(snap);
  renderSettings(snap);
  renderAbout(snap);
  renderSessions(snap);
}

async function refresh(quiet) {
  try {
    store.snap = await invoke('snapshot');
    render();
  } catch (e) {
    if (!quiet) toast(`读取状态失败：${e}`, 'bad');
  }
}

/* ------------------------------------------------------------------ */
/* 倒计时：本地每 0.5 秒推进，每 10 秒与后端对一次                       */
/* ------------------------------------------------------------------ */

let tickCount = 0;

function startCountdown() {
  if (store.countdownTimer) clearInterval(store.countdownTimer);
  store.countdownTimer = setInterval(() => {
    const snap = store.snap;
    if (!snap) return;
    const c = snap.countdown;
    if (c.seconds > 0) {
      c.seconds -= 1;
      c.progress = c.total_secs > 0
        ? Math.min(1, Math.max(0, (c.total_secs - c.seconds) / c.total_secs))
        : 0;
      $('ring-time').textContent = hms(c.seconds);
      $('ring-value').style.strokeDashoffset = ringOffset(c.progress);
    }
    tickCount += 1;
    // 每 10 秒（或刚好到点）与后端对齐一次真实的计划状态。
    if (tickCount % 20 === 0 || c.seconds <= 0) refresh(true);
  }, 500);
}

/* ------------------------------------------------------------------ */
/* 动作                                                                */
/* ------------------------------------------------------------------ */

async function withBusy(button, work) {
  if (store.busy) return;
  store.busy = true;
  const label = button ? button.textContent : null;
  if (button) { button.disabled = true; button.textContent = '处理中…'; }
  try {
    await work();
  } catch (e) {
    toast(String(e && e.message ? e.message : e), 'bad');
  } finally {
    store.busy = false;
    if (button) { button.disabled = false; button.textContent = label; }
    await refresh(true);
  }
}

async function saveSettings(patch) {
  try {
    const result = await invoke('set_settings', { patch });
    store.snap = result.snapshot;
    render();
    toast(result.message, 'ok');
  } catch (e) {
    toast(String(e), 'bad');
  }
}

$('btn-night').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  const text = await invoke('night_now', { mode: null });
  toast(text || '已进入夜间，远程仍可操作', 'ok');
}));

$('btn-day').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  const text = await invoke('day_now');
  toast(text || '已恢复显示', 'ok');
}));

$('btn-pause').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  const text = await invoke('pause_schedule');
  toast(text || '今天不会再自动黑屏', 'ok');
}));

$('btn-test').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  const text = await invoke('test_mode', { seconds: 15, mode: null });
  toast(`${text}。现在请用 UU / ToDesk 连一下，确认能看到并操作画面。`, 'ok');
}));

$('btn-refresh').addEventListener('click', () => withBusy(null, () => refresh(false)));

$('btn-go-onboarding').addEventListener('click', () => openWizard());

$('btn-save-schedule').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  await saveSettings({ start: $('in-start').value, end: $('in-end').value });
}));

document.querySelectorAll('[data-preset]').forEach((button) => {
  button.addEventListener('click', () => {
    const [start, end] = button.dataset.preset.split(',');
    $('in-start').value = start;
    $('in-end').value = end;
  });
});

$('sw-enabled').addEventListener('click', async (event) => {
  const node = event.currentTarget;
  const next = !isSwitchOn('sw-enabled');
  if (next) {
    try {
      const text = await invoke('activate_schedule');
      toast(text || '每日计划已启用', 'ok');
      await refresh(false);
    } catch (e) {
      toast(String(e), 'bad');
      await refresh(true);
    }
  } else {
    try {
      const text = await invoke('pause_schedule');
      toast(text || '已暂停每日计划', 'ok');
      await refresh(false);
    } catch (e) {
      toast(String(e), 'bad');
    }
  }
  node.blur();
});

const SWITCH_PATCHES = {
  'sw-autostart': { autostart: null },
  'sw-strict': { strict_local_input: null },
  'sw-lock': { suppress_lock: null },
  'sw-awake': { keep_awake: null },
  'sw-perf': { high_performance_at_night: null },
};

Object.entries(SWITCH_PATCHES).forEach(([id, template]) => {
  $(id).addEventListener('click', async (event) => {
    const node = event.currentTarget;
    const next = !isSwitchOn(id);
    node.setAttribute('aria-checked', next ? 'true' : 'false');
    const key = Object.keys(template)[0];
    await saveSettings({ [key]: next });
    node.blur();
  });
});

$('btn-save-chord').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  const raw = $('in-chord').value.trim();
  if (!raw) {
    toast('组合键不能为空', 'bad');
    return;
  }
  const keys = raw.split('+').map((part) => part.trim()).filter(Boolean);
  const hold = Number($('in-hold').value);
  await saveSettings({ chord: keys, hold_ms: Number.isFinite(hold) ? hold : 1200 });
}));

$('btn-repair').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  openWizard(true);
  await runWizard(true);
}));

$('btn-diag').addEventListener('click', (event) => withBusy(event.currentTarget, exportDiag));
$('btn-diag2').addEventListener('click', (event) => withBusy(event.currentTarget, exportDiag));

async function exportDiag() {
  const path = await invoke('export_diag');
  toast(`诊断包已生成：${path}（已脱敏：路径、用户名、令牌都换成占位符）`, 'ok');
  try { await invoke('reveal', { path }); } catch (e) { /* 定位失败不算错 */ }
}

$('btn-selfcheck').addEventListener('click', (event) => withBusy(event.currentTarget, async () => {
  await refresh(false);
  toast('自检结果已刷新，完整内容在「关于」页和诊断包里。', 'ok');
}));

$('btn-open-home').addEventListener('click', async () => {
  if (!store.snap) return;
  try { await invoke('reveal', { path: store.snap.home }); } catch (e) { toast(String(e), 'bad'); }
});

$('btn-open-logs').addEventListener('click', async () => {
  if (!store.snap) return;
  try { await invoke('reveal', { path: store.snap.logs_dir }); } catch (e) { toast(String(e), 'bad'); }
});

$('btn-log-refresh').addEventListener('click', () => {
  const file = store.logIndex && store.logIndex.files.find((f) => f.path === store.currentLog);
  if (file) openLog(file);
  else refreshLogs();
});

$('btn-log-reveal').addEventListener('click', async () => {
  if (!store.currentLog) return;
  try { await invoke('reveal', { path: store.currentLog }); } catch (e) { toast(String(e), 'bad'); }
});

/* 日志页的两个 tab */
document.querySelectorAll('.tab').forEach((tab) => {
  tab.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach((t) => t.classList.toggle('active', t === tab));
    document.querySelectorAll('.tab-panel').forEach((panel) => {
      panel.hidden = panel.dataset.panel !== tab.dataset.tab;
    });
    if (tab.dataset.tab === 'files') refreshLogs();
  });
});

/* ------------------------------------------------------------------ */
/* 安装向导                                                            */
/* ------------------------------------------------------------------ */

let wizardRunning = false;

async function openWizard(forceRepair) {
  $('wizard').hidden = false;
  const view = await invoke('onboarding_state');
  renderWizard(view);
  // 第一次打开时只显示"开始"按钮：让用户先读到"会弹一次管理员确认"，再点下去。
  const started = view.steps.some((s) => s.status !== 'pending');
  $('wizard-start').hidden = started;
  $('wizard-retry').hidden = view.failed === 0;
}

function renderWizard(view) {
  const box = $('wizard-steps');
  box.replaceChildren();
  view.steps.forEach((step, index) => {
    const node = el('div', `wstep ${step.status === 'pending' ? '' : step.status}`);
    node.dataset.step = step.id;
    node.appendChild(el('div', 'wstep-num', step.status === 'ok' ? '✓'
      : step.status === 'failed' || step.status === 'manual' ? '!' : String(index + 1)));
    const body = el('div');
    const title = el('div', 'wstep-title');
    title.appendChild(document.createTextNode(step.title));
    if (step.required) title.appendChild(el('span', 'req', '必需'));
    body.appendChild(title);
    const detail = el('div', 'wstep-detail', step.message || step.detail);
    body.appendChild(detail);
    if (step.action) {
      const actions = el('div', 'wstep-actions');
      const hint = el('code', null, step.action);
      hint.style.fontSize = '11.5px';
      hint.style.color = 'var(--text-dim)';
      hint.style.userSelect = 'text';
      actions.appendChild(hint);
      body.appendChild(actions);
    }
    node.appendChild(body);
    node.appendChild(el('div', 'wstep-status', STATUS_TEXT[step.status] || ''));
    box.appendChild(node);
  });

  const failed = view.failed;
  $('wizard-note').textContent = view.completed
    ? `向导已完成（${view.completed_at || '—'}）。需要重新检查时点「重试失败的步骤」。`
    : failed > 0
      ? `还有 ${failed} 个步骤需要处理。照着每一条下面的命令做，或者点「重试失败的步骤」。`
      : view.log_hint;
  // "完成"按钮只在所有必需步骤都处理完时可以点；勾选框决定能不能真的结束。
  const ready = view.completed || failed === 0;
  $('wizard-finish').hidden = !ready;
  $('wizard-retry').hidden = failed === 0;
  $('wizard-confirm').checked = !!view.remote_confirmed;
  $('wizard-finish').disabled = !$('wizard-confirm').checked;
}

async function runWizard(repair) {
  if (wizardRunning) return;
  wizardRunning = true;
  $('wizard-start').disabled = true;
  $('wizard-retry').disabled = true;
  $('wizard-title').textContent = repair ? '重新授权 / 修复' : '安装向导';
  $('wizard-sub').textContent = repair
    ? '只重跑失败或需要处理的步骤。'
    : '只弹一次管理员确认，之后夜间切换不再打扰。';
  try {
    const view = repair
      ? await invoke('repair')
      : await invoke('run_setup', { installVdd: true, selfTestSeconds: 15 });
    renderWizard(view);
    if (view.failed === 0) toast('向导步骤都处理完了。确认远程能连之后点「完成」。', 'ok');
    else toast(`还有 ${view.failed} 个步骤需要处理，看向导里的说明。`, 'bad');
  } catch (e) {
    toast(String(e), 'bad');
  } finally {
    wizardRunning = false;
    $('wizard-start').disabled = false;
    $('wizard-retry').disabled = false;
    await refresh(true);
  }
}

listen('setup-step', (event) => {
  const payload = event.payload || {};
  document.querySelectorAll('.wstep').forEach((node) => {
    const isCurrent = node.dataset.step === payload.id;
    node.classList.toggle('running', isCurrent);
    if (isCurrent) {
      const detail = node.querySelector('.wstep-detail');
      if (detail) detail.textContent = payload.text;
      const status = node.querySelector('.wstep-status');
      if (status) status.textContent = '进行中';
    }
  });
});

$('wizard-start').addEventListener('click', () => runWizard(false));
$('wizard-retry').addEventListener('click', () => runWizard(true));
$('wizard-close').addEventListener('click', () => { $('wizard').hidden = true; });
$('wizard-confirm').addEventListener('change', async (event) => {
  const confirmed = !!event.currentTarget.checked;
  try {
    const view = await invoke('confirm_remote', { confirmed });
    $('wizard-finish').disabled = !confirmed;
    $('wizard-note').textContent = confirmed
      ? (view.completed ? '向导已完成。' : '可以点「完成」结束向导了。')
      : view.log_hint;
  } catch (e) {
    toast(String(e), 'bad');
  }
});

$('wizard-finish').addEventListener('click', async () => {
  try {
    await invoke('confirm_remote', { confirmed: true });
    const message = await invoke('finish_onboarding');
    toast(message ? '向导完成，不会再自动弹出。' : '向导完成。', 'ok');
    $('wizard').hidden = true;
    await refresh(false);
  } catch (e) {
    toast(String(e), 'bad');
  }
});

/* ------------------------------------------------------------------ */
/* 启动                                                                */
/* ------------------------------------------------------------------ */

(async function boot() {
  primeRing();
  const hash = location.hash.replace('#', '');
  showPage(hash || 'home');
  await refresh(false);
  startCountdown();

  // 第一次启动（向导没走完）时自动把向导打开。
  if (store.snap && !store.snap.onboarding.completed) {
    openWizard();
  }
})();

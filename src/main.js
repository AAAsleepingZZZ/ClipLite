/* ============================================================
 * ClipLite 前端主逻辑
 * 依赖：Tauri 2（window.__TAURI__），无构建链，vanilla JS
 * 数据流：
 *   启动 / 窗口可见 / clipboard-updated 事件 → get_items() 刷新列表
 *   单击条目 → copy_item() → 150ms 后 hide_panel()
 *   双击条目 → paste_item() → hide_panel()
 * ============================================================ */
(() => {
  'use strict';

  /* ---------- 常量 ---------- */

  // 后端 IPC 命令名（与 Rust 侧契约严格一致）
  const CMD = {
    GET_ITEMS: 'get_items',
    COPY_ITEM: 'copy_item',
    PASTE_ITEM: 'paste_item',
    TOGGLE_PIN: 'toggle_pin',
    DELETE_ITEM: 'delete_item',
    CLEAR_HISTORY: 'clear_history',
    HIDE_PANEL: 'hide_panel',
    GET_IMAGE: 'get_image',
    GET_SETTINGS: 'get_settings',
    UPDATE_SETTINGS: 'update_settings',
    GET_SNIPPETS: 'get_snippets',
    ADD_SNIPPET: 'add_snippet',
    UPDATE_SNIPPET: 'update_snippet',
    DELETE_SNIPPET: 'delete_snippet',
    RENAME_GROUP: 'rename_group',
    DELETE_GROUP: 'delete_group',
    ADD_GROUP: 'add_group',
    GET_GROUPS: 'get_groups',
    COPY_TEXT: 'copy_text',
    PASTE_TEXT: 'paste_text',
    GET_FILE_THUMB: 'get_file_thumb',
  };

  const HIDE_DELAY = 150;        // 单击复制后延迟隐藏的毫秒数（给双击留判定窗口）
  const SEARCH_DEBOUNCE = 150;   // 搜索输入防抖
  const TOAST_DURATION = 1600;   // 轻提示展示时长
  const PREVIEW_MAX_LINES = 5;   // 文本预览最多展开行数，超出以省略号收尾

  // 动态图标（固定于 JS 内，避免模板拼接风险）
  const ICONS = {
    STAR:
      '<svg viewBox="0 0 24 24" fill="currentColor"><polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/></svg>',
    IMAGE:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="2" ry="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/></svg>',
    FILE:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>',
    SNIPPET:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/><rect x="8" y="2" width="8" height="4" rx="1" ry="1"/></svg>',
    PLUS:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>',
    MORE:
      '<svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7"/><circle cx="12" cy="12" r="1.7"/><circle cx="19" cy="12" r="1.7"/></svg>',
  };

  /* ---------- 状态 ---------- */

  const state = {
    demoMode: false,        // 非 Tauri 环境（普通浏览器预览）
    items: [],              // 当前列表数据
    query: '',              // 当前搜索词
    imageCache: new Map(),  // id -> base64 dataURL 缓存
    currentHotkey: '',      // 已保存的全局热键
    pendingHotkey: null,    // 本次会话新捕获、尚未保存的热键
    capturing: false,       // 是否处于按键捕获中
    selId: null,            // 键盘导航当前选中的条目 id
    demoItems: [],          // 演示数据（仅 demo 模式）
    demoSettings: null,     // 演示设置（仅 demo 模式）
    activeTab: 'history',   // 当前视图：history | snippets
    sourceFilter: '',       // 来源筛选（空 = 全部），仅历史视图
    snippets: [],           // 片段库数据
    groups: [],             // 分组名列表（独立持久化，含空分组）
    groupFilter: '',        // 分组筛选（空 = 全部），仅片段库视图
    snippetSelId: null,     // 片段库键盘选中项 id
  };

  // 演示数据：3 条，覆盖置顶 / 多行文本 / 图片三种形态
  const DEMO_ITEMS = () => {
    const now = Date.now();
    return [
      {
        id: 1,
        kind: 'text',
        content: '欢迎使用 ClipLite\n复制的内容会实时出现在这里\n支持搜索、置顶与一键粘贴',
        imagePath: null,
        pinned: true,
        createdAt: now - 3 * 60000,
        sourceApp: 'chrome.exe',
        sourceTitle: '知乎 - Chrome',
      },
      {
        id: 2,
        kind: 'text',
        content: 'https://github.com/tauri-apps/tauri —— Tauri 官方仓库',
        imagePath: null,
        pinned: false,
        createdAt: now - 2 * 3600000,
        sourceApp: 'Code.exe',
        sourceTitle: 'settings.json - ClipLite',
      },
      {
        id: 3,
        kind: 'image',
        content: null,
        imagePath: '演示图片.png',
        pinned: false,
        createdAt: now - 26 * 3600000,
        sourceApp: 'mspaint.exe',
        sourceTitle: '未命名 - 画图',
      },
    ];
  };

  const DEMO_SNIPPETS = () => [
    { id: 1, content: '祝好！\n此致\n张三', title: '邮件落款', groupName: '工作', createdAt: Date.now() - 86400000 },
    { id: 2, content: '192.168.1.1', title: null, groupName: '默认', createdAt: Date.now() - 2 * 86400000 },
  ];

  const DEMO_SETTINGS = { hotkey: 'Ctrl+Shift+V', clearOnExit: false, autostart: false, glassAlpha: 0.46, maxItems: 500, maxImageMb: 10 };

  /* ---------- DOM 引用 ---------- */

  const $ = (id) => document.getElementById(id);

  const el = {
    app: $('app'),
    glassBg: $('glass-bg'),
    envHint: $('env-hint'),
    searchInput: $('search-input'),
    clearBtn: $('clear-btn'),
    settingsBtn: $('settings-btn'),
    tabBar: $('tab-bar'),
    tabHistory: $('tab-history'),
    tabSnippets: $('tab-snippets'),
    tabIndicator: $('tab-indicator'),
    sourceBar: $('source-bar'),
    sourceChips: $('source-chips'),
    list: $('list'),
    pinnedGroup: $('pinned-group'),
    pinnedList: $('pinned-list'),
    historyGroup: $('history-group'),
    historyList: $('history-list'),
    emptyState: $('empty-state'),
    snippetView: $('snippet-view'),
    groupChips: $('group-chips'),
    snippetList: $('snippet-list'),
    snippetEmpty: $('snippet-empty'),
    addSnippetBtn: $('add-snippet-btn'),
    menu: $('context-menu'),
    menuPinLabel: $('context-menu').querySelector('[data-action="pin"] span'),
    menuSaveSnippet: $('menu-save-snippet'),
    backdrop: $('modal-backdrop'),
    hotkeyBtn: $('hotkey-btn'),
    hotkeyCancel: $('hotkey-cancel'),
    autostartSwitch: $('autostart-switch'),
    clearSwitch: $('clear-switch'),
    glassRange: $('glass-range'),
    glassVal: $('glass-val'),
    maxItemsRange: $('max-items-range'),
    maxItemsVal: $('max-items-val'),
    maxImageRange: $('max-image-range'),
    maxImageVal: $('max-image-val'),
    hotkeyError: $('hotkey-error'),
    modalClose: $('modal-close'),
    modalSave: $('modal-save'),
    snippetBackdrop: $('snippet-backdrop'),
    snippetModalTitle: $('snippet-modal-title'),
    snippetTitleInput: $('snippet-title-input'),
    snippetContentInput: $('snippet-content-input'),
    snippetGroupSelect: $('snippet-group-select'),
    snippetModalCancel: $('snippet-modal-cancel'),
    snippetModalSave: $('snippet-modal-save'),
    groupBackdrop: $('group-backdrop'),
    groupModalTitle: $('group-modal-title'),
    groupNameInput: $('group-name-input'),
    groupModalDelete: $('group-modal-delete'),
    groupModalCancel: $('group-modal-cancel'),
    groupModalSave: $('group-modal-save'),
    toast: $('toast'),
  };

  /* ---------- 工具函数 ---------- */

  // IPC 调用入口：demo 模式走本地模拟，真实环境走 Tauri
  async function call(cmd, args) {
    if (!state.demoMode) {
      return window.__TAURI__.core.invoke(cmd, args || {});
    }
    return demoCall(cmd, args);
  }

  // demo 模式下的本地模拟：让浏览器预览也能走通全部交互
  function demoCall(cmd, args) {
    const a = args || {};
    switch (cmd) {
      case CMD.GET_ITEMS: {
        const q = (a.query || '').trim().toLowerCase();
        if (!q) return [...state.demoItems];
        const terms = q.split(/\s+/).filter(Boolean);
        return state.demoItems.filter((it) =>
          it.content && terms.some((t) => it.content.toLowerCase().includes(t))
        );
      }
      case CMD.COPY_ITEM:
        toast('演示模式：已复制');
        return null;
      case CMD.PASTE_ITEM:
        toast('演示模式：已粘贴');
        return null;
      case CMD.COPY_TEXT:
        toast('演示模式：已复制文本');
        return null;
      case CMD.PASTE_TEXT:
        toast('演示模式：已粘贴文本');
        return null;
      case CMD.TOGGLE_PIN: {
        const it = state.demoItems.find((i) => i.id === a.id);
        if (it) it.pinned = !it.pinned;
        return null;
      }
      case CMD.DELETE_ITEM:
        state.demoItems = state.demoItems.filter((i) => i.id !== a.id);
        return null;
      case CMD.CLEAR_HISTORY:
        state.demoItems = [];
        return null;
      case CMD.GET_IMAGE:
        return null; // 无真实图片，缩略图保持占位
      case CMD.GET_FILE_THUMB:
        return null; // 无真实文件，保持占位
      case CMD.GET_SETTINGS:
        return { ...state.demoSettings };
      case CMD.UPDATE_SETTINGS:
        Object.assign(state.demoSettings, a.settings || {});
        return null;
      case CMD.GET_SNIPPETS:
        return [...state.demoSnippets];
      case CMD.GET_GROUPS: {
        const groups = [...new Set(state.demoSnippets.map((s) => s.groupName))];
        if (!groups.includes('默认')) groups.push('默认');
        return groups;
      }
      case CMD.ADD_GROUP:
        if (state.demoSnippets.some((s) => s.groupName === a.name)) {
          throw new Error(`分组「${a.name}」已存在`);
        }
        return null;
      case CMD.ADD_SNIPPET: {
        const s = {
          id: Math.max(0, ...state.demoSnippets.map((x) => x.id)) + 1,
          content: a.content,
          title: a.title || null,
          groupName: a.groupName || '默认',
          createdAt: Date.now(),
        };
        state.demoSnippets.push(s);
        return s;
      }
      case CMD.UPDATE_SNIPPET: {
        const s = state.demoSnippets.find((x) => x.id === a.id);
        if (s) {
          if (a.content !== undefined) s.content = a.content;
          if (a.title !== undefined) s.title = a.title || null;
          if (a.groupName !== undefined) s.groupName = a.groupName;
        }
        return s;
      }
      case CMD.DELETE_SNIPPET:
        state.demoSnippets = state.demoSnippets.filter((x) => x.id !== a.id);
        return null;
      case CMD.RENAME_GROUP:
        state.demoSnippets.forEach((s) => {
          if (s.groupName === a.oldName) s.groupName = a.newName;
        });
        return null;
      case CMD.DELETE_GROUP:
        state.demoSnippets.forEach((s) => {
          if (s.groupName === a.name) s.groupName = '默认';
        });
        return null;
      default:
        return null; // hide_panel 等命令无操作
    }
  }

  function escapeRegExp(s) {
    return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }

  function debounce(fn, ms) {
    let t = null;
    return (...args) => {
      clearTimeout(t);
      t = setTimeout(() => fn(...args), ms);
    };
  }

  function clamp(v, min, max) {
    return Math.min(Math.max(v, min), max);
  }

  function pad2(n) {
    return String(n).padStart(2, '0');
  }

  // 相对时间：刚刚 / N分钟前 / N小时前 / 昨天 / 日期
  function formatTime(ts) {
    const diff = Date.now() - ts;
    const min = Math.floor(diff / 60000);
    if (min < 1) return '刚刚';
    if (min < 60) return `${min} 分钟前`;
    const d = new Date(ts);
    const now = new Date();
    if (d.toDateString() === now.toDateString()) {
      return `${Math.floor(diff / 3600000)} 小时前`;
    }
    const yesterday = new Date(now);
    yesterday.setDate(now.getDate() - 1);
    if (d.toDateString() === yesterday.toDateString()) return '昨天';
    return formatDate(d, now);
  }

  // 跨年日期补年份，如「8月12日」/「2025年8月12日」
  function formatDate(d, now) {
    const md = `${d.getMonth() + 1}月${d.getDate()}日`;
    return d.getFullYear() === now.getFullYear()
      ? md
      : `${d.getFullYear()}年${md}`;
  }

  // 图片条目占位标题：今天显示「图片 14:32」，更早显示日期
  function formatImageLabel(ts) {
    const d = new Date(ts);
    const now = new Date();
    if (d.toDateString() === now.toDateString()) {
      return `图片 ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
    }
    const yesterday = new Date(now);
    yesterday.setDate(now.getDate() - 1);
    if (d.toDateString() === yesterday.toDateString()) return '图片 昨天';
    return `图片 ${formatDate(d, now)}`;
  }

  // 全局热键格式化：Ctrl/Alt/Shift + 主键，如「Ctrl+Shift+V」
  // 特殊键名映射到 Tauri accelerator 使用的写法
  const KEY_ALIAS = {
    ' ': 'Space',
    ArrowUp: 'Up',
    ArrowDown: 'Down',
    ArrowLeft: 'Left',
    ArrowRight: 'Right',
  };

  function formatHotkey(e) {
    const mods = [];
    if (e.ctrlKey) mods.push('Ctrl');
    if (e.altKey) mods.push('Alt');
    if (e.shiftKey) mods.push('Shift');
    const key = e.key;
    // 输入法组合键 / 未知键不参与捕获
    if (!key || key === 'Process' || key === 'Unidentified') return null;
    // 纯修饰键按下：等待主键
    if (['Control', 'Alt', 'Shift', 'Meta'].includes(key)) return null;
    let main = KEY_ALIAS[key] || key;
    if (main.length === 1) main = main.toUpperCase();
    return [...mods, main].join('+');
  }

  /* ---------- 轻提示 ---------- */

  let toastTimer = null;

  function toast(msg) {
    el.toast.textContent = msg;
    el.toast.hidden = false;
    requestAnimationFrame(() => el.toast.classList.add('show'));
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      el.toast.classList.remove('show');
      setTimeout(() => {
        el.toast.hidden = true;
      }, 150);
    }, TOAST_DURATION);
  }

  /* ---------- 列表渲染 ---------- */

  // 从后端拉取列表（带当前搜索词），丢弃过期响应
  let refreshSeq = 0;

  async function refresh() {
    const seq = ++refreshSeq;
    let items;
    try {
      const q = state.query.trim();
      items = await call(CMD.GET_ITEMS, q ? { query: q } : {});
    } catch (err) {
      console.error('获取剪贴板列表失败：', err);
      return;
    }
    if (seq !== refreshSeq) return; // 已有更新的请求，丢弃本次结果
    state.items = items || [];
    renderSourceChips();
    render(filteredItems());
  }

  const debouncedRefresh = debounce(refresh, SEARCH_DEBOUNCE);

  // 来源筛选栏：从当前数据聚合应用名（去重 + 计数），渲染 chips
  function renderSourceChips() {
    const counts = new Map();
    for (const it of state.items) {
      if (it.sourceApp) counts.set(it.sourceApp, (counts.get(it.sourceApp) || 0) + 1);
    }
    const apps = [...counts.keys()].sort((a, b) => a.localeCompare(b));
    const frag = document.createDocumentFragment();

    const all = document.createElement('button');
    all.className = 'chip' + (state.sourceFilter ? '' : ' active');
    all.dataset.source = '';
    all.textContent = '全部';
    frag.append(all);

    for (const app of apps) {
      const chip = document.createElement('button');
      chip.className = 'chip' + (state.sourceFilter === app ? ' active' : '');
      chip.dataset.source = app;
      const name = document.createElement('span');
      name.textContent = app;
      const count = document.createElement('span');
      count.className = 'chip-count';
      count.textContent = String(counts.get(app));
      chip.append(name, count);
      frag.append(chip);
    }
    el.sourceChips.replaceChildren(frag);
    el.sourceBar.hidden = apps.length === 0 && !state.sourceFilter;
  }

  // 应用来源筛选（仅本地过滤，不重新请求后端）
  function filteredItems() {
    if (!state.sourceFilter) return state.items;
    return state.items.filter((it) => it.sourceApp === state.sourceFilter);
  }

  function render(items) {
    const pinned = items.filter((it) => it.pinned);
    const history = items.filter((it) => !it.pinned);

    // 置顶分组：无置顶时整组隐藏
    el.pinnedGroup.hidden = pinned.length === 0;
    el.pinnedList.replaceChildren(...pinned.map(buildItem));

    el.historyGroup.hidden = history.length === 0;
    el.historyList.replaceChildren(...history.map(buildItem));

    // 空状态：区分「无记录」与「搜索无结果」
    el.emptyState.hidden = items.length > 0;
    el.emptyState.textContent = state.query.trim()
      ? '未找到匹配的记录'
      : state.sourceFilter
        ? '该应用暂无复制记录'
        : '暂无复制记录';

    applySelection();
  }

  /* ---------- 键盘导航选中态 ---------- */

  // 当前视图的可见条目列表（DOM 顺序）
  function visibleItems() {
    const container = state.activeTab === 'snippets' ? el.snippetList : el.list;
    return Array.from(container.querySelectorAll('.item')).map(
      (li) => Number(li.dataset.id),
    );
  }

  // 当前视图的选中 id（历史 / 片段库各自独立）
  function getSelId() {
    return state.activeTab === 'snippets' ? state.snippetSelId : state.selId;
  }

  function setSelId(v) {
    if (state.activeTab === 'snippets') state.snippetSelId = v;
    else state.selId = v;
  }

  // 高亮选中条目；选中项不存在（列表刷新/清空）时自动回落到第一条
  function applySelection() {
    const ids = visibleItems();
    const sel = getSelId();
    if (ids.length === 0) {
      setSelId(null);
    } else if (!ids.includes(sel)) {
      setSelId(ids[0]);
    }
    const container = state.activeTab === 'snippets' ? el.snippetList : el.list;
    container.querySelectorAll('.item').forEach((li) => {
      li.classList.toggle('selected', Number(li.dataset.id) === getSelId());
    });
  }

  // 方向键移动选中：按 DOM 顺序循环
  function moveSelection(delta) {
    const ids = visibleItems();
    if (ids.length === 0) return;
    const idx = ids.indexOf(getSelId());
    const next = (idx < 0 ? (delta > 0 ? -1 : 0) : idx + delta) % ids.length;
    setSelId(ids[(next + ids.length) % ids.length]);
    applySelection();
    const container = state.activeTab === 'snippets' ? el.snippetList : el.list;
    const li = container.querySelector(`.item[data-id="${getSelId()}"]`);
    li?.scrollIntoView({ block: 'nearest' });
  }

  function buildItem(item) {
    const li = document.createElement('li');
    li.className = 'item';
    li.dataset.id = String(item.id);
    if (item.pinned) li.dataset.pinned = 'true';

    // ---- 图片条目：64x64 缩略图 + 占位标题 ----
    if (item.kind === 'image') {
      li.classList.add('item-image');

      const thumb = document.createElement('div');
      thumb.className = 'item-thumb';
      const placeholder = document.createElement('span');
      placeholder.className = 'thumb-ph';
      placeholder.innerHTML = ICONS.IMAGE;
      thumb.append(placeholder);
      li.append(thumb);

      const main = document.createElement('div');
      main.className = 'item-main';
      const label = document.createElement('div');
      label.className = 'item-image-label';
      label.textContent = formatImageLabel(item.createdAt);
      main.append(label);
      li.append(main);

      if (!state.demoMode) loadThumb(item, thumb);
      return li;
    }

    // ---- 文件条目：图片文件显示缩略图，其余显示文件图标 ----
    if (item.kind === 'file') {
      const paths = (item.content || '').split('\n').filter(Boolean);
      const fileName = paths[0] ? paths[0].split(/[\\/]/).pop() : '文件';
      const isImageFile = /\.(png|jpe?g|gif|webp|bmp)$/i.test(paths[0] || '');

      if (isImageFile) {
        li.classList.add('item-file', 'item-file-img');
        const thumb = document.createElement('div');
        thumb.className = 'item-thumb';
        const placeholder = document.createElement('span');
        placeholder.className = 'thumb-ph';
        placeholder.innerHTML = ICONS.IMAGE;
        thumb.append(placeholder);
        li.append(thumb);

        const main = document.createElement('div');
        main.className = 'item-main';
        const name = document.createElement('div');
        name.className = 'item-file-name';
        name.textContent = paths.length > 1 ? `${fileName} 等 ${paths.length} 项` : fileName;
        name.title = paths.join('\n');
        main.append(name);
        const sub = document.createElement('div');
        sub.className = 'item-file-path';
        sub.textContent = paths.length > 1 ? `${paths.length} 个文件` : paths[0] || '';
        main.append(sub);
        li.append(main);
        if (!state.demoMode) loadFileThumb(item, thumb);
        return li;
      }

      const icon = document.createElement('div');
      icon.className = 'item-file-icon';
      icon.innerHTML = ICONS.FILE;
      li.append(icon);

      const main = document.createElement('div');
      main.className = 'item-main';
      const name = document.createElement('div');
      name.className = 'item-file-name';
      name.textContent = paths.length > 1 ? `${fileName} 等 ${paths.length} 项` : fileName;
      name.title = paths.join('\n');
      main.append(name);
      const sub = document.createElement('div');
      sub.className = 'item-file-path';
      sub.textContent = paths.length > 1 ? `${paths.length} 个文件` : paths[0] || '';
      main.append(sub);
      li.append(main);
      return li;
    }

    // ---- 文本条目：单行截断预览 + 来源 + 时间戳 ----
    li.classList.add('item-text');
    const main = document.createElement('div');
    main.className = 'item-main';
    const preview = document.createElement('div');
    preview.className = 'item-preview';
    preview.append(buildPreview(item.content || ''));
    main.append(preview);

    // 来源次级行（仅记录到来源时显示）
    if (item.sourceApp) {
      const src = document.createElement('div');
      src.className = 'item-source';
      src.textContent = item.sourceTitle ? `${item.sourceApp} · ${item.sourceTitle}` : item.sourceApp;
      src.title = src.textContent;
      main.append(src);
    }
    li.append(main);

    const meta = document.createElement('div');
    meta.className = 'item-meta';
    if (item.pinned) {
      const star = document.createElement('span');
      star.className = 'item-star';
      star.innerHTML = ICONS.STAR;
      star.title = '已置顶';
      meta.append(star);
    }
    const time = document.createElement('span');
    time.className = 'item-time';
    time.textContent = formatTime(item.createdAt);
    meta.append(time);
    li.append(meta);

    return li;
  }

  // 预览文本：多行折叠为一行，换行处以 ⏎ 标记；命中搜索词加 <mark> 高亮
  function buildPreview(content) {
    const frag = document.createDocumentFragment();
    const query = state.query.trim();
    const lines = content.split('\n');
    const shown = lines.slice(0, PREVIEW_MAX_LINES);
    const truncated = lines.length > PREVIEW_MAX_LINES;

    shown.forEach((line, i) => {
      if (i > 0) frag.append(nlSpan());
      frag.append(highlightLine(line, query));
    });
    if (truncated) {
      const tail = document.createElement('span');
      tail.className = 'item-nl';
      tail.textContent = '…';
      frag.append(tail);
    }
    return frag;
  }

  function nlSpan() {
    const s = document.createElement('span');
    s.className = 'item-nl';
    s.textContent = '⏎';
    return s;
  }

  function highlightLine(line, query) {
    const frag = document.createDocumentFragment();
    if (!query) {
      frag.append(document.createTextNode(line));
      return frag;
    }
    const terms = query.split(/\s+/).filter(Boolean).map(escapeRegExp);
    if (!terms.length) {
      frag.append(document.createTextNode(line));
      return frag;
    }
    const re = new RegExp(`(${terms.join('|')})`, 'gi');
    let last = 0;
    for (const m of line.matchAll(re)) {
      if (m.index > last) frag.append(document.createTextNode(line.slice(last, m.index)));
      const mark = document.createElement('mark');
      mark.textContent = m[0];
      frag.append(mark);
      last = m.index + m[0].length;
    }
    if (last < line.length) frag.append(document.createTextNode(line.slice(last)));
    return frag;
  }

  // 图片缩略图：get_image 取 base64 后渲染，Map 缓存
  async function loadThumb(item, thumbEl) {
    try {
      let url = state.imageCache.get(item.id);
      if (!url) {
        url = await call(CMD.GET_IMAGE, { id: item.id });
        if (url) state.imageCache.set(item.id, url);
      }
      if (!url) return; // 无数据则保持占位
      const img = new Image();
      img.onload = () => {
        // onload 时列表可能已刷新，丢弃过期渲染
        if (!thumbEl.isConnected) return;
        img.classList.add('item-thumb-img');
        thumbEl.replaceChildren(img);
      };
      img.onerror = () => {
        /* 加载失败保持占位图标 */
      };
      img.src = url;
    } catch (err) {
      console.error(`图片加载失败（id=${item.id}）：`, err);
    }
  }

  // 图片文件条目的缩略图：get_file_thumb 生成 128x128 缩略图（独立缓存）
  async function loadFileThumb(item, thumbEl) {
    try {
      let url = state.imageCache.get(`f${item.id}`);
      if (!url) {
        url = await call(CMD.GET_FILE_THUMB, { id: item.id });
        if (url) state.imageCache.set(`f${item.id}`, url);
      }
      if (!url) return; // 非图片文件/失败则保持占位
      const img = new Image();
      img.onload = () => {
        if (!thumbEl.isConnected) return;
        img.classList.add('item-thumb-img');
        thumbEl.replaceChildren(img);
      };
      img.onerror = () => {
        /* 加载失败保持占位图标 */
      };
      img.src = url;
    } catch (err) {
      console.error(`文件缩略图加载失败（id=${item.id}）：`, err);
    }
  }

  function getItem(li) {
    const id = Number(li.dataset.id);
    return state.items.find((it) => it.id === id) || null;
  }

  function getSnippet(li) {
    const id = Number(li.dataset.id);
    return state.snippets.find((s) => s.id === id) || null;
  }

  /* ---------- 片段库 ---------- */

  // 片段库数据源（全量 + 本地过滤：搜索词 / 分组）
  function filteredSnippets() {
    const q = state.query.trim().toLowerCase();
    return state.snippets
      .filter((s) => !state.groupFilter || s.groupName === state.groupFilter)
      .filter((s) => {
        if (!q) return true;
        const hay = `${s.title || ''}\n${s.content}`.toLowerCase();
        return q.split(/\s+/).filter(Boolean).some((t) => hay.includes(t));
      });
  }

  // 全量拉取片段库 + 分组列表 → 渲染分组 chips + 列表
  async function refreshSnippets() {
    let snippets, groups;
    try {
      [snippets, groups] = await Promise.all([
        call(CMD.GET_SNIPPETS),
        call(CMD.GET_GROUPS),
      ]);
    } catch (err) {
      console.error('获取片段库失败：', err);
      return;
    }
    state.snippets = snippets || [];
    state.groups = groups || [];
    // 兜底：片段里出现的分组但不在分组表中时补进内存（旧数据兼容）
    for (const s of state.snippets) {
      if (!state.groups.includes(s.groupName)) state.groups.push(s.groupName);
    }
    renderGroupChips();
    renderSnippetList();
  }

  const debouncedRefreshSnippets = debounce(refreshSnippets, SEARCH_DEBOUNCE);

  // 分组 chips：全部 + 各分组（来自独立分组表）+ 新建分组按钮
  function renderGroupChips() {
    const groups = [...state.groups].sort((a, b) => a.localeCompare(b, 'zh-CN'));
    const frag = document.createDocumentFragment();

    const all = document.createElement('button');
    all.className = 'chip' + (state.groupFilter ? '' : ' active');
    all.dataset.group = '';
    all.textContent = '全部';
    frag.append(all);

    for (const g of groups) {
      const chip = document.createElement('button');
      chip.className = 'chip' + (state.groupFilter === g ? ' active' : '');
      chip.dataset.group = g;
      const name = document.createElement('span');
      name.textContent = g;
      const count = document.createElement('span');
      count.className = 'chip-count';
      count.textContent = String(state.snippets.filter((s) => s.groupName === g).length);
      chip.append(name, count);
      chip.title = '点击筛选，右侧 ⋮ 管理分组';
      const more = document.createElement('span');
      more.className = 'chip-more';
      more.innerHTML = ICONS.MORE;
      more.title = '管理分组（重命名 / 删除）';
      more.dataset.group = g;
      chip.append(more);
      frag.append(chip);
    }

    const add = document.createElement('button');
    add.className = 'chip add';
    add.dataset.group = '__add__';
    add.innerHTML = ICONS.PLUS;
    add.title = '新建分组';
    frag.append(add);

    el.groupChips.replaceChildren(frag);
  }

  // 渲染片段列表
  function renderSnippetList() {
    const items = filteredSnippets();
    el.snippetList.replaceChildren(...items.map(buildSnippetItem));
    el.snippetEmpty.hidden = items.length > 0;
    el.snippetEmpty.textContent = state.query.trim()
      ? '未找到匹配的片段'
      : state.groupFilter
        ? '该分组暂无片段'
        : '暂无片段，右键历史条目或点击新建';
    applySelection();
  }

  function buildSnippetItem(s) {
    const li = document.createElement('li');
    li.className = 'item item-snippet';
    li.dataset.id = String(s.id);
    li.dataset.snippet = 'true';

    const icon = document.createElement('div');
    icon.className = 'item-snippet-icon';
    icon.innerHTML = ICONS.SNIPPET;
    li.append(icon);

    const main = document.createElement('div');
    main.className = 'item-main';
    const title = document.createElement('div');
    title.className = 'item-snippet-title';
    title.textContent = s.title || firstLine(s.content) || '（空片段）';
    title.title = s.content;
    main.append(title);
    const preview = document.createElement('div');
    preview.className = 'item-snippet-preview';
    const text = s.title ? s.content : collapsePreview(s.content);
    preview.textContent = text || '';
    main.append(preview);
    li.append(main);

    const meta = document.createElement('div');
    meta.className = 'item-meta';
    const time = document.createElement('span');
    time.className = 'item-time';
    time.textContent = formatTime(s.createdAt);
    meta.append(time);
    const more = document.createElement('button');
    more.className = 'item-more-btn';
    more.innerHTML = ICONS.MORE;
    more.title = '更多操作（复制 / 编辑 / 删除）';
    more.dataset.action = 'more';
    meta.append(more);
    li.append(meta);

    return li;
  }

  // 内容首行（多行片段取第一行作为标题）
  function firstLine(content) {
    const line = (content || '').split('\n').find((l) => l.trim()) || '';
    return line.length > 24 ? line.slice(0, 24) + '…' : line;
  }

  // 多行折叠为单行预览（⏎ 标记换行）
  function collapsePreview(content) {
    return content.split('\n').slice(0, 3).join(' ⏎ ');
  }

  // 片段粘贴（与历史条目同路径：写剪贴板 → 模拟 Ctrl+V 到原窗口）
  function pasteSnippet(s) {
    if (!s) return;
    call(CMD.PASTE_TEXT, { content: s.content }).catch((err) =>
      console.error('片段粘贴失败：', err)
    );
  }

  /* ---------- 单击 / 双击 ---------- */

  let hideTimer = null;

  // 单击：复制并延迟隐藏；双击会先清掉这个定时器再走粘贴
  function onItemClick(item) {
    state.selId = item.id;
    applySelection();
    call(CMD.COPY_ITEM, { id: item.id }).catch((err) =>
      console.error('复制失败：', err)
    );
    clearTimeout(hideTimer);
    hideTimer = setTimeout(
      () => call(CMD.HIDE_PANEL).catch(() => {}),
      HIDE_DELAY
    );
  }

  function onItemDblClick(item) {
    clearTimeout(hideTimer);
    call(CMD.PASTE_ITEM, { id: item.id }).catch((err) =>
      console.error('粘贴失败：', err)
    );
    call(CMD.HIDE_PANEL).catch(() => {});
  }

  // 片段单击：复制到剪贴板；双击：粘贴
  function onSnippetClick(s) {
    state.snippetSelId = s.id;
    applySelection();
    call(CMD.COPY_TEXT, { content: s.content }).catch((err) =>
      console.error('复制片段失败：', err)
    );
    clearTimeout(hideTimer);
    hideTimer = setTimeout(
      () => call(CMD.HIDE_PANEL).catch(() => {}),
      HIDE_DELAY
    );
  }

  function onSnippetDblClick(s) {
    clearTimeout(hideTimer);
    pasteSnippet(s);
    call(CMD.HIDE_PANEL).catch(() => {});
  }

  // 键盘粘贴：选中项或第一条
  function pasteSelected() {
    if (state.activeTab === 'snippets') {
      const target =
        state.snippets.find((s) => s.id === state.snippetSelId) ||
        filteredSnippets()[0];
      if (target) pasteSnippet(target);
      return;
    }
    const target =
      state.items.find((it) => it.id === state.selId) || state.items[0];
    if (target) onItemDblClick(target);
  }

  /* ---------- 右键菜单 ---------- */

  let menuTarget = null; // 当前菜单对应的条目元素
  let menuIsSnippet = false; // 菜单是否针对片段条目

  function openMenu(x, y, targetLi, isSnippet) {
    menuTarget = targetLi;
    menuIsSnippet = !!isSnippet;
    if (isSnippet) {
      el.menuPinLabel.textContent = '编辑';
      el.menuSaveSnippet.hidden = true;
    } else {
      el.menuPinLabel.textContent =
        targetLi.dataset.pinned === 'true' ? '取消置顶' : '置顶';
      // 仅文本条目可存为片段
      const item = getItem(targetLi);
      el.menuSaveSnippet.hidden = !(item && item.kind === 'text');
    }
    el.menu.hidden = false;
    requestAnimationFrame(() => {
      const rect = el.menu.getBoundingClientRect();
      el.menu.style.left = `${clamp(x, 8, window.innerWidth - rect.width - 8)}px`;
      el.menu.style.top = `${clamp(y, 8, window.innerHeight - rect.height - 8)}px`;
      el.menu.classList.add('open');
    });
  }

  function closeMenu() {
    el.menu.classList.remove('open');
    menuTarget = null;
  }

  async function handleMenuAction(action, id) {
    if (menuIsSnippet) {
      const s = state.snippets.find((x) => x.id === id);
      if (!s) return;
      if (action === 'copy') {
        onSnippetClick(s);
      } else if (action === 'pin') {
        // 片段库的"编辑"入口
        openSnippetModal(s);
      } else if (action === 'delete') {
        try {
          await call(CMD.DELETE_SNIPPET, { id });
          toast('片段已删除');
        } catch (err) {
          console.error('删除片段失败：', err);
        }
        refreshSnippets();
      }
      return;
    }
    if (action === 'copy') {
      const item = state.items.find((it) => it.id === id);
      if (item) onItemClick(item);
    } else if (action === 'pin') {
      try {
        await call(CMD.TOGGLE_PIN, { id });
      } catch (err) {
        console.error('切换置顶失败：', err);
      }
      refresh();
    } else if (action === 'save-snippet') {
      const item = state.items.find((it) => it.id === id);
      if (item) openSnippetModal(null, item.content || '');
    } else if (action === 'delete') {
      try {
        await call(CMD.DELETE_ITEM, { id });
        state.imageCache.delete(id); // 顺带清理图片缓存
      } catch (err) {
        console.error('删除失败：', err);
      }
      refresh();
    }
  }

  /* ---------- 设置弹窗 ---------- */

  const modalOpen = () => el.backdrop.classList.contains('open');

  // 毛玻璃透出强度：百分比(0-100) ↔ 染色不透明度(0.30-0.62)
  const GLASS_MAX = 0.62;
  const GLASS_MIN = 0.30;
  const pctToAlpha = (p) => GLASS_MAX - (p / 100) * (GLASS_MAX - GLASS_MIN);
  const alphaToPct = (a) =>
    Math.round(((GLASS_MAX - a) / (GLASS_MAX - GLASS_MIN)) * 100);

  // 应用透出强度：写入 CSS 变量（实时生效）+ 同步控件状态
  function applyGlassAlpha(pct) {
    const alpha = pctToAlpha(pct);
    document.documentElement.style.setProperty(
      '--glass-alpha',
      alpha.toFixed(3)
    );
    el.glassRange.value = pct;
    el.glassRange.style.setProperty('--fill', `${pct}%`);
    el.glassVal.textContent = `${pct}%`;
  }

  async function openModal() {
    hideHotkeyError();
    stopCapture();
    state.pendingHotkey = null;
    el.hotkeyBtn.textContent = '加载中…';
    el.hotkeyBtn.classList.remove('pending');
    el.backdrop.hidden = false;
    // 双 rAF：确保 display 切换后入场过渡生效
    requestAnimationFrame(() =>
      requestAnimationFrame(() => el.backdrop.classList.add('open'))
    );
    try {
      const s = await call(CMD.GET_SETTINGS);
      state.currentHotkey = s.hotkey || '';
      el.hotkeyBtn.textContent = s.hotkey || '未设置';
      el.autostartSwitch.checked = !!s.autostart;
      el.clearSwitch.checked = !!s.clearOnExit;
      applyGlassAlpha(alphaToPct(typeof s.glassAlpha === 'number' ? s.glassAlpha : 0.46));
      const maxItems = typeof s.maxItems === 'number' ? s.maxItems : 500;
      applyRangeVal(el.maxItemsRange, el.maxItemsVal, maxItems, '条');
      const maxImage = typeof s.maxImageMb === 'number' ? s.maxImageMb : 10;
      applyRangeVal(el.maxImageRange, el.maxImageVal, maxImage, 'MB');
    } catch (err) {
      console.error('读取设置失败：', err);
      el.hotkeyBtn.textContent = '未设置';
    }
  }

  // 容量滑动条：数值显示 + 轨道填充
  function applyRangeVal(range, valEl, v, unit) {
    range.value = v;
    range.style.setProperty('--fill', `${((v - range.min) / (range.max - range.min)) * 100}%`);
    valEl.textContent = `${v} ${unit}`;
  }

  // 容量滑动条数值脉冲动效
  function bumpVal(valEl) {
    valEl.classList.remove('bump');
    void valEl.offsetWidth;
    valEl.classList.add('bump');
  }

  function closeModal() {
    stopCapture();
    el.backdrop.classList.remove('open');
    setTimeout(() => {
      el.backdrop.hidden = true;
    }, 170); // 等退场过渡结束再移除 DOM
  }

  // 进入按键捕获：等待用户按下组合键
  function startCapture() {
    state.capturing = true;
    el.hotkeyBtn.textContent = '请按下快捷键…';
    el.hotkeyBtn.classList.add('capturing');
    el.hotkeyCancel.hidden = false;
    hideHotkeyError();
    if (document.activeElement === el.searchInput) el.searchInput.blur();
  }

  function stopCapture() {
    state.capturing = false;
    el.hotkeyBtn.classList.remove('capturing');
    el.hotkeyCancel.hidden = true;
  }

  function showHotkeyError(msg) {
    el.hotkeyError.textContent = msg;
    el.hotkeyError.hidden = false;
  }

  function hideHotkeyError() {
    el.hotkeyError.hidden = true;
  }

  async function saveSettings() {
    const settings = {
      autostart: el.autostartSwitch.checked,
      clearOnExit: el.clearSwitch.checked,
      maxItems: Number(el.maxItemsRange.value),
      maxImageMb: Number(el.maxImageRange.value),
    };
    if (state.pendingHotkey) settings.hotkey = state.pendingHotkey;
    el.modalSave.disabled = true;
    try {
      await call(CMD.UPDATE_SETTINGS, { settings });
      if (state.pendingHotkey) state.currentHotkey = state.pendingHotkey;
      state.pendingHotkey = null;
      closeModal();
      toast('设置已保存');
    } catch (err) {
      console.error('保存设置失败：', err);
      // 热键注册失败：弹提示，并回退显示已保存的键位
      const msg = (err && err.message) || String(err);
      showHotkeyError(`热键注册失败：${msg}`);
      state.pendingHotkey = null;
      el.hotkeyBtn.textContent = state.currentHotkey || '未设置';
      el.hotkeyBtn.classList.remove('pending');
    } finally {
      el.modalSave.disabled = false;
    }
  }

  /* ---------- 片段编辑弹窗 ---------- */

  let editingSnippetId = null; // 当前编辑的片段 id（null = 新建）
  let snippetModalPromise = null;

  // 分组名列表（供下拉与分组管理，来自独立分组表）
  function groupNames() {
    return [...state.groups].sort((a, b) => a.localeCompare(b, 'zh-CN'));
  }

  // 渲染分组下拉选项（保留当前选中）
  function fillGroupSelect(selected) {
    const groups = groupNames();
    if (!groups.includes('默认')) groups.unshift('默认');
    el.snippetGroupSelect.replaceChildren(
      ...groups.map(
        (g) =>
          new Option(g, g, g === selected, g === selected)
      )
    );
  }

  // 打开片段编辑弹窗：edit 为 null 时新建；content 用于历史条目「存为片段」预填
  function openSnippetModal(edit, prefillContent) {
    editingSnippetId = edit ? edit.id : null;
    el.snippetModalTitle.textContent = edit ? '编辑片段' : '新建片段';
    el.snippetTitleInput.value = edit ? edit.title || '' : '';
    el.snippetContentInput.value = prefillContent || (edit ? edit.content : '');
    fillGroupSelect(edit ? edit.groupName : state.groupFilter || '默认');
    el.snippetBackdrop.hidden = false;
    requestAnimationFrame(() =>
      requestAnimationFrame(() => el.snippetBackdrop.classList.add('open'))
    );
    if (!edit && !prefillContent) el.snippetTitleInput.focus();
    else el.snippetContentInput.focus();
    return new Promise((resolve) => {
      snippetModalPromise = resolve;
    });
  }

  function closeSnippetModal() {
    el.snippetBackdrop.classList.remove('open');
    setTimeout(() => {
      el.snippetBackdrop.hidden = true;
    }, 170);
  }

  async function saveSnippetModal() {
    const content = el.snippetContentInput.value.trim();
    if (!content) {
      toast('片段内容不能为空');
      el.snippetContentInput.focus();
      return;
    }
    const title = el.snippetTitleInput.value.trim() || null;
    const groupName = el.snippetGroupSelect.value || '默认';
    try {
      if (editingSnippetId != null) {
        await call(CMD.UPDATE_SNIPPET, {
          id: editingSnippetId,
          content,
          title,
          groupName,
        });
        toast('片段已更新');
      } else {
        await call(CMD.ADD_SNIPPET, { content, title, groupName });
        toast('片段已保存');
      }
      snippetModalPromise && snippetModalPromise(true);
      snippetModalPromise = null;
      closeSnippetModal();
      refreshSnippets();
    } catch (err) {
      console.error('保存片段失败：', err);
      toast(`保存失败：${(err && err.message) || err}`);
    }
  }

  /* ---------- 分组管理弹窗 ---------- */

  let editingGroupName = null; // 当前编辑的分组名（null = 新建）

  // 打开分组弹窗：新建（oldName=null）或重命名
  function openGroupModal(oldName) {
    editingGroupName = oldName;
    el.groupModalTitle.textContent = oldName ? `重命名「${oldName}」` : '新建分组';
    el.groupNameInput.value = oldName || '';
    // 删除按钮仅在重命名已有分组时显示（「默认」组不可删）
    el.groupModalDelete.hidden = !(oldName && oldName !== '默认');
    el.groupBackdrop.hidden = false;
    requestAnimationFrame(() =>
      requestAnimationFrame(() => el.groupBackdrop.classList.add('open'))
    );
    el.groupNameInput.focus();
    el.groupNameInput.select();
  }

  function closeGroupModal() {
    el.groupBackdrop.classList.remove('open');
    setTimeout(() => {
      el.groupBackdrop.hidden = true;
    }, 170);
  }

  // 提交分组操作（新建或重命名）
  async function submitGroupModal() {
    const name = el.groupNameInput.value.trim();
    if (!name) {
      toast('分组名不能为空');
      return;
    }
    const oldName = editingGroupName;
    closeGroupModal();
    if (oldName) {
      try {
        await call(CMD.RENAME_GROUP, { oldName, newName: name });
        toast('分组已重命名');
        if (state.groupFilter === oldName) state.groupFilter = name;
      } catch (err) {
        toast(`重命名失败：${(err && err.message) || err}`);
      }
    } else {
      try {
        await call(CMD.ADD_GROUP, { name });
        toast('分组已创建');
        // 新建分组：切换到该分组筛选，方便往里面存片段
        state.groupFilter = name;
      } catch (err) {
        toast(`创建失败：${(err && err.message) || err}`);
      }
    }
    refreshSnippets();
  }

  // 删除分组：组内片段移入「默认」组（删除按钮在分组弹窗内，无需 confirm）
  function deleteGroup(name) {
    call(CMD.DELETE_GROUP, { name })
      .then(() => {
        toast('分组已删除，片段已移入默认组');
        if (state.groupFilter === name) state.groupFilter = '';
        closeGroupModal();
        refreshSnippets();
      })
      .catch((err) => toast(`删除失败：${(err && err.message) || err}`));
  }

  /* ---------- Tab 切换 ---------- */

  function switchTab(tab) {
    if (state.activeTab === tab) return;
    state.activeTab = tab;
    const isSnippets = tab === 'snippets';

    el.tabHistory.classList.toggle('active', !isSnippets);
    el.tabHistory.setAttribute('aria-selected', String(!isSnippets));
    el.tabSnippets.classList.toggle('active', isSnippets);
    el.tabSnippets.setAttribute('aria-selected', String(isSnippets));
    el.list.hidden = isSnippets;
    el.snippetView.hidden = !isSnippets;
    el.sourceBar.hidden = isSnippets;

    // 搜索框 placeholder 随视图变化；切换时保留搜索词但重置选中
    el.searchInput.placeholder = isSnippets ? '搜索片段…' : '搜索剪贴板…';
    state.selId = null;
    state.snippetSelId = null;
    updateTabIndicator();
    if (isSnippets) {
      refreshSnippets();
      el.searchInput.focus();
    } else {
      renderSourceChips();
      refresh();
    }
  }

  // 滑动指示条：平移到激活 Tab（tab-bar 三元素：历史 / 片段库 / 指示条）
  function updateTabIndicator() {
    const bar = el.tabBar;
    const active = state.activeTab === 'snippets' ? el.tabSnippets : el.tabHistory;
    const barRect = bar.getBoundingClientRect();
    const btnRect = active.getBoundingClientRect();
    const x = btnRect.left - barRect.left;
    el.tabIndicator.style.transform = `translateX(${x - 3}px)`;
  }

  /* ---------- 事件绑定 ---------- */

  function bindEvents() {
    // 搜索：实时过滤（防抖）+ 清空按钮显隐
    el.searchInput.addEventListener('input', () => {
      state.query = el.searchInput.value;
      el.clearBtn.hidden = !state.query;
      if (state.activeTab === 'snippets') debouncedRefreshSnippets();
      else debouncedRefresh();
    });

    el.clearBtn.addEventListener('click', () => {
      el.searchInput.value = '';
      state.query = '';
      el.clearBtn.hidden = true;
      el.searchInput.focus();
      if (state.activeTab === 'snippets') refreshSnippets();
      else refresh();
    });

    el.settingsBtn.addEventListener('click', openModal);

    // 透出强度：拖动实时预览（脉冲动效），松手自动保存
    el.glassRange.addEventListener('input', () => {
      const pct = Number(el.glassRange.value);
      applyGlassAlpha(pct);
      el.glassVal.classList.remove('bump');
      void el.glassVal.offsetWidth;
      el.glassVal.classList.add('bump');
    });
    el.glassRange.addEventListener('change', () => {
      const alpha = pctToAlpha(Number(el.glassRange.value));
      call(CMD.UPDATE_SETTINGS, { settings: { glassAlpha: alpha } })
        .then(() => toast('透出强度已更新'))
        .catch((err) => console.error('保存透出强度失败：', err));
    });

    // 容量滑动条：拖动实时预览数值（脉冲动效），保存时随设置一起提交
    const bindCapacityRange = (range, valEl, unit) => {
      range.addEventListener('input', () => {
        applyRangeVal(range, valEl, Number(range.value), unit);
        bumpVal(valEl);
      });
    };
    bindCapacityRange(el.maxItemsRange, el.maxItemsVal, '条');
    bindCapacityRange(el.maxImageRange, el.maxImageVal, 'MB');

    // 列表交互：事件委托到列表容器
    el.list.addEventListener('click', (e) => {
      const li = e.target.closest('.item');
      if (!li) return;
      const item = getItem(li);
      if (item) onItemClick(item);
    });

    el.list.addEventListener('dblclick', (e) => {
      const li = e.target.closest('.item');
      if (!li) return;
      const item = getItem(li);
      if (item) onItemDblClick(item);
    });

    // 片段列表交互
    el.snippetList.addEventListener('click', (e) => {
      // ⋮ 按钮：打开更多操作菜单（复制/编辑/删除），不触发单击复制
      const moreBtn = e.target.closest('.item-more-btn');
      if (moreBtn) {
        // 阻止冒泡：避免 document 的"点击外部关闭"在菜单显示前把它关掉
        e.stopPropagation();
        const li = moreBtn.closest('.item');
        if (li) {
          openMenu(
            moreBtn.getBoundingClientRect().right - 4,
            moreBtn.getBoundingClientRect().bottom + 4,
            li,
            true
          );
        }
        return;
      }
      const li = e.target.closest('.item');
      if (!li) return;
      const s = getSnippet(li);
      if (s) onSnippetClick(s);
    });

    el.snippetList.addEventListener('dblclick', (e) => {
      if (e.target.closest('.item-more-btn')) return; // ⋮ 按钮不触发双击粘贴
      const li = e.target.closest('.item');
      if (!li) return;
      const s = getSnippet(li);
      if (s) onSnippetDblClick(s);
    });

    // 右键菜单：历史条目与片段条目都拦截默认菜单
    document.addEventListener('contextmenu', (e) => {
      const histLi = e.target.closest('#list .item');
      if (histLi) {
        e.preventDefault();
        openMenu(e.clientX, e.clientY, histLi, false);
        return;
      }
      const snipLi = e.target.closest('#snippet-list .item');
      if (snipLi) {
        e.preventDefault();
        openMenu(e.clientX, e.clientY, snipLi, true);
      }
    });

    // 分组 chip 右键：打开分组管理弹窗（重命名 / 删除）
    el.groupChips.addEventListener('contextmenu', (e) => {
      const chip = e.target.closest('.chip[data-group]');
      if (!chip || chip.dataset.group === '' || chip.dataset.group === '__add__') return;
      e.preventDefault();
      openGroupModal(chip.dataset.group);
    });

    // 来源筛选 chips 点击
    el.sourceChips.addEventListener('click', (e) => {
      const chip = e.target.closest('.chip[data-source]');
      if (!chip) return;
      state.sourceFilter = chip.dataset.source === '' ? '' : chip.dataset.source;
      renderSourceChips();
      render(filteredItems());
    });

    // 分组 chips 点击（全部 / 具体分组 / 新建 / ⋮ 管理分组）
    el.groupChips.addEventListener('click', (e) => {
      // ⋮ 按钮：打开分组管理弹窗（重命名 / 删除）
      const more = e.target.closest('.chip-more');
      if (more) {
        const g = more.dataset.group;
        if (g) openGroupModal(g);
        return;
      }
      const chip = e.target.closest('.chip[data-group]');
      if (!chip) return;
      if (chip.dataset.group === '__add__') {
        openGroupModal(null);
        return;
      }
      state.groupFilter = chip.dataset.group === '' ? '' : chip.dataset.group;
      renderGroupChips();
      renderSnippetList();
    });

    // 分组管理弹窗
    el.groupBackdrop.addEventListener('click', (e) => {
      if (e.target === el.groupBackdrop) closeGroupModal();
    });
    el.groupModalCancel.addEventListener('click', closeGroupModal);
    el.groupModalSave.addEventListener('click', submitGroupModal);
    el.groupModalDelete.addEventListener('click', () => {
      if (editingGroupName) deleteGroup(editingGroupName);
    });
    el.groupNameInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        submitGroupModal();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        closeGroupModal();
      }
    });

    // Tab 切换
    el.tabHistory.addEventListener('click', () => switchTab('history'));
    el.tabSnippets.addEventListener('click', () => switchTab('snippets'));
    el.addSnippetBtn.addEventListener('click', () => openSnippetModal(null));

    // 片段编辑弹窗
    el.snippetBackdrop.addEventListener('click', (e) => {
      if (e.target === el.snippetBackdrop) {
        snippetModalPromise && snippetModalPromise(false);
        snippetModalPromise = null;
        closeSnippetModal();
      }
    });
    el.snippetModalCancel.addEventListener('click', () => {
      snippetModalPromise && snippetModalPromise(false);
      snippetModalPromise = null;
      closeSnippetModal();
    });
    el.snippetModalSave.addEventListener('click', saveSnippetModal);

    // 点击菜单外部关闭
    document.addEventListener('click', (e) => {
      if (menuTarget && !e.target.closest('.context-menu')) closeMenu();
    });

    el.menu.addEventListener('click', (e) => {
      const btn = e.target.closest('.menu-item');
      if (!btn) return;
      const id = menuTarget ? Number(menuTarget.dataset.id) : null;
      closeMenu();
      if (id != null) handleMenuAction(btn.dataset.action, id);
    });

    // 设置弹窗
    el.backdrop.addEventListener('click', (e) => {
      if (e.target === el.backdrop) closeModal();
    });
    el.modalClose.addEventListener('click', closeModal);
    el.modalSave.addEventListener('click', saveSettings);
    el.hotkeyBtn.addEventListener('click', () => {
      if (state.capturing) stopCapture();
      else startCapture();
    });
    el.hotkeyCancel.addEventListener('click', stopCapture);

    // 全局按键：热键捕获优先，其次 Esc
    document.addEventListener('keydown', (e) => {
      if (state.capturing) {
        e.preventDefault();
        e.stopPropagation();
        const hk = formatHotkey(e);
        if (!hk) return; // 纯修饰键按下，继续等待主键
        state.pendingHotkey = hk;
        el.hotkeyBtn.textContent = hk;
        el.hotkeyBtn.classList.add('pending');
        stopCapture();
        return;
      }
      // 片段编辑弹窗打开时：Esc 关闭，回车保存
      if (!el.snippetBackdrop.hidden) {
        if (e.key === 'Escape') {
          e.preventDefault();
          snippetModalPromise && snippetModalPromise(false);
          snippetModalPromise = null;
          closeSnippetModal();
        } else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
          e.preventDefault();
          saveSnippetModal();
        }
        return;
      }
      if (modalOpen()) return; // 设置弹窗打开时不响应列表快捷键

      // Tab：在历史 / 片段库视图间切换（替代默认的焦点循环）
      if (e.key === 'Tab') {
        e.preventDefault();
        switchTab(state.activeTab === 'snippets' ? 'history' : 'snippets');
        return;
      }

      // 方向键：上/下移动选中（含搜索框聚焦时）
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        moveSelection(e.key === 'ArrowDown' ? 1 : -1);
        return;
      }
      // 回车：粘贴选中条目（或第一条）
      if (e.key === 'Enter') {
        e.preventDefault();
        pasteSelected();
        return;
      }
      if (e.key === 'Escape') {
        if (modalOpen()) closeModal();
        else call(CMD.HIDE_PANEL).catch(() => {});
      }
    });
  }

  /* ---------- 数据订阅 ---------- */

  // 剪贴板更新事件：实时刷新列表
  async function subscribeClipboard() {
    try {
      await window.__TAURI__.event.listen('clipboard-updated', () => refresh());
    } catch (err) {
      console.error('订阅 clipboard-updated 事件失败：', err);
    }
  }

  // 面板背景：后端在显示前截取屏幕并推送，前端作为毛玻璃背景
  async function subscribePanelBackground() {
    try {
      await window.__TAURI__.event.listen('panel-background', (e) => {
        if (typeof e.payload === 'string') {
          el.glassBg.style.backgroundImage = `url("${e.payload}")`;
          el.app.classList.add('glass-on');
        }
        // 重放入场动画：每次呼出都"呼吸"一次
        el.app.classList.remove('panel-open');
        void el.app.offsetWidth;
        el.app.classList.add('panel-open');
        // 自动选中第一条（回车即可粘贴）
        state.selId = null;
        state.snippetSelId = null;
        applySelection();
      });
    } catch (err) {
      console.error('订阅 panel-background 事件失败：', err);
    }
  }

  // 窗口获得焦点（面板被唤起）时刷新
  async function subscribeWindowFocus() {
    try {
      const { getCurrentWindow } = window.__TAURI__.window;
      if (getCurrentWindow) {
        await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
          if (focused) {
            if (state.activeTab === 'snippets') {
              refreshSnippets();
            } else {
              refresh();
            }
            el.searchInput.focus();
          }
        });
      }
    } catch (err) {
      console.error('订阅窗口焦点事件失败：', err);
    }
    // WebView 可见性变化兜底
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) refresh();
    });
  }

  /* ---------- 启动 ---------- */

  async function init() {
    // 环境检测：__TAURI__ 不存在则进入演示模式
    const tauri = window.__TAURI__;
    state.demoMode = !(tauri && tauri.core && tauri.core.invoke);

    if (state.demoMode) {
      state.demoItems = DEMO_ITEMS();
      state.demoSnippets = DEMO_SNIPPETS();
      state.demoSettings = { ...DEMO_SETTINGS };
      el.envHint.hidden = false;
    }

    bindEvents();
    await refresh();
    await refreshSnippets();
    updateTabIndicator();

    if (!state.demoMode) {
      subscribeClipboard();
      subscribePanelBackground();
      subscribeWindowFocus();
      // 启动时应用已保存的透出强度（未保存过则用 CSS 默认值）
      call(CMD.GET_SETTINGS)
        .then((s) => {
          if (s && typeof s.glassAlpha === 'number') {
            applyGlassAlpha(alphaToPct(s.glassAlpha));
          }
        })
        .catch(() => {});
    }

    // 唤起后聚焦搜索框，可直接输入过滤
    el.searchInput.focus();
  }

  init();
})();

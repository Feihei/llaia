function llaiaApp() {
  return {
    tab: 'config',
    token: localStorage.getItem('llaia_token') || '',
    // auth state: unchecked / checking / passed / failed
    authed: false,
    authing: false,
    authError: '',
    // chat
    messages: [],
    inputText: '',
    busy: false,
    uploaded: [],
    // todo (ADR-0024, read-only display; v1 no click-to-toggle from UI)
    todos: [],
    questions: [],
    // 长期目标（ADR-0021，只读展示）
    goal: null,
    _todoTimer: null,
    ws: null,
    // config
    cfg: { runtime:{}, log:{}, provider:{}, agent:{}, webui:{}, channels:{qq:{},telegram:{},dingtalk:{},wechat:{},mail:{},feishu:{}}, tools:{terminal:{whitelist:[]},tavily:{},tts:{}} },
    compatOpen: {},
    configSection: 'runtime',
    // 支持的 channel 卡片元数据（参数表单由这些字段驱动渲染）。
    // 顺序即展示顺序；WebUI 另有独立卡片、永远排在最前。
    channelCards: [
      {
        key: 'qq', icon: '💬', title: 'QQ',
        desc: 'QQ 开放平台机器人（扫码/手动登录），长轮询免公网回调。',
        fields: [
          { name: 'app_id', label: 'app_id' },
          { name: 'app_secret', label: 'app_secret', password: true },
          { name: 'confirm_mode', label: 'confirm_mode', placeholder: 'none' },
          { name: 'owner_openid', label: 'owner_openid', placeholder: 'optional cron push target' },
        ],
      },
      {
        key: 'telegram', icon: '✈️', title: 'Telegram',
        desc: 'BotFather 机器人 + long polling，免公网回调。',
        fields: [
          { name: 'bot_token', label: 'bot_token', password: true },
          { name: 'allow_chat_id', label: 'allow_chat_id', type: 'number', placeholder: '0 = 不限制' },
          { name: 'owner_chat_id', label: 'owner_chat_id', type: 'number', placeholder: '0 = 回退 allow_chat_id' },
          { name: 'api_base', label: 'api_base', placeholder: 'https://api.telegram.org' },
        ],
      },
      {
        key: 'dingtalk', icon: '📌', title: 'DingTalk',
        desc: '钉钉开放平台机器人 + Stream Mode WebSocket，免公网回调。',
        fields: [
          { name: 'client_id', label: 'client_id' },
          { name: 'client_secret', label: 'client_secret' },
          { name: 'allow_staff_id', label: 'allow_staff_id', placeholder: '空 = 不限制' },
          { name: 'api_base', label: 'api_base', placeholder: 'https://api.dingtalk.com' },
        ],
      },
      {
        key: 'wechat', icon: '🟢', title: 'WeChat',
        desc: '微信 ClawBot（ilink bot），扫码登录 + 长轮询免公网回调。',
        fields: [
          { name: 'allow_user_id', label: 'allow_user_id', placeholder: '空 = 不限制' },
          { name: 'owner_user_id', label: 'owner_user_id', placeholder: 'optional cron push target' },
          { name: 'base_url', label: 'base_url', placeholder: 'https://ilinkai.weixin.qq.com' },
          { name: 'cdn_base_url', label: 'cdn_base_url', placeholder: 'https://novac2c.cdn.weixin.qq.com/c2c' },
        ],
      },
      {
        key: 'feishu', icon: '🚀', title: 'Feishu / Lark',
        desc: '飞书开放平台事件订阅「长连接」模式（WebSocket 免公网回调）。',
        fields: [
          { name: 'app_id', label: 'app_id' },
          { name: 'app_secret', label: 'app_secret', password: true },
          { name: 'allow_open_id', label: 'allow_open_id', placeholder: '空 = 不限制' },
          { name: 'mention_only', label: 'mention_only（群内仅 @ 时回复）', type: 'checkbox' },
          { name: 'api_base', label: 'api_base', placeholder: 'https://open.feishu.cn/open-apis' },
          { name: 'ws_base', label: 'ws_base', placeholder: 'https://open.feishu.cn' },
        ],
      },
      {
        key: 'mail', icon: '✉️', title: 'Mail',
        desc: 'IMAP 收件 + SMTP 发信（个人助理入口，单用户安全锁）。',
        fields: [
          { name: 'imap_server', label: 'imap_server', placeholder: 'imap.gmail.com' },
          { name: 'imap_port', label: 'imap_port', type: 'number', placeholder: '993' },
          { name: 'imap_user', label: 'imap_user' },
          { name: 'imap_pass', label: 'imap_pass', password: true },
          { name: 'smtp_server', label: 'smtp_server', placeholder: 'smtp.gmail.com' },
          { name: 'smtp_port', label: 'smtp_port', type: 'number', placeholder: '465' },
          { name: 'smtp_user', label: 'smtp_user', placeholder: '留空复用 imap_user' },
          { name: 'smtp_pass', label: 'smtp_pass', password: true, placeholder: '留空复用 imap_pass' },
          { name: 'poll_interval_secs', label: 'poll_interval_secs', type: 'number', placeholder: '30' },
          { name: 'mailbox', label: 'mailbox', placeholder: 'INBOX' },
          { name: 'owner_email', label: 'owner_email', placeholder: '只响应此地址（谨慎留空）' },
          { name: 'from_name', label: 'from_name', placeholder: 'LLAIA' },
          { name: 'mark_seen', label: 'mark_seen（处理后标记已读）', type: 'checkbox' },
          { name: 'max_attachment_mb', label: 'max_attachment_mb', type: 'number', placeholder: '10' },
        ],
      },
    ],
    rawToml: '',
    rawMsg: '',
    // about
    status: null,
    restarting: false,
    restartMsg: '',
    shuttingDown: false,
    shutdownMsg: '',
    // cron
    cronSection: 'tasks',
    cronTasks: [],
    cronHistory: [],
    cronRaw: '',
    cronMsg: '',
    cronRawMsg: '',
    _cronEditor: null,
    // mcp
    mcpSection: 'servers',
    mcpServers: [],
    mcpRaw: '',
    mcpMsg: '',
    mcpRawMsg: '',
    mcpTesting: null,
    _mcpEditor: null,
    // skills
    skills: [],
    skillMsg: '',
    skillEditing: null,
    skillContent: '',
    skillContentMsg: '',
    // per-agent fallback draft (dropdown selection before "Add")
    fallbackDraft: {},
    // 模型探测（P5 W2）
    probing: null,
    probeMsg: {},
    probeModels: {},
    probeChecked: {},
    // 会话历史（P5 W1）
    sessions: [],
    selectedSession: null,
    sessionDetail: null,
    sessionMsg: '',

    async init() {
      // 标记已成功进入初始化，供 index.html 的启动失败兜底检测使用
      window.__llaiaBooted = true;
      // prefer URL query token, then localStorage
      const urlParams = new URLSearchParams(location.search);
      const urlToken = urlParams.get('token');
      if (urlToken) {
        this.token = urlToken;
        localStorage.setItem('llaia_token', urlToken);
      }
      // verify if token exists, otherwise show login
      if (this.token) {
        await this.verifyToken();
        // 默认进入 Config 页时，自动拉取配置
        if (this.authed && this.tab === 'config') {
          this.switchConfig();
        }
        // 规划后执行（ADR-0024）：只读轮询当前会话 todo 清单
        if (this.authed) {
          this.loadTodos();
          this._todoTimer = setInterval(() => this.loadTodos(), 5000);
          // ask_user（ADR-0022）：只读轮询待回答问题
          this.loadQuestions();
          this._questionTimer = setInterval(() => this.loadQuestions(), 5000);
          // 长期目标（ADR-0021）：只读轮询 goal.md 状态
          this.loadGoal();
          this._goalTimer = setInterval(() => this.loadGoal(), 5000);
        }
      }
    },
    async loadTodos() {
      try {
        const r = await this.apiFetch('/api/todos');
        if (r.ok) {
          const j = await r.json();
          this.todos = j.todos || [];
        }
      } catch (e) { /* 非致命：UI 静默跳过 */ }
    },
    async loadQuestions() {
      try {
        const r = await this.apiFetch('/api/questions');
        if (r.ok) {
          const j = await r.json();
          this.questions = j.questions || [];
        }
      } catch (e) { /* 非致命：UI 静默跳过 */ }
    },
    async loadGoal() {
      try {
        const r = await this.apiFetch('/api/goal');
        if (r.ok) {
          const j = await r.json();
          this.goal = j.goal || null;
        }
      } catch (e) { /* 非致命：UI 静默跳过 */ }
    },

    // ---- 会话历史（P5 W1） ----
    async switchSessions() {
      this.tab = 'sessions';
      if (this.sessions.length === 0) await this.loadSessions();
    },
    async loadSessions() {
      try {
        const r = await this.apiFetch('/api/sessions?limit=200');
        if (r.ok) {
          const j = await r.json();
          this.sessions = j.sessions || [];
          this.sessionMsg = '';
        }
      } catch (e) {
        this.sessionMsg = 'Failed to load sessions: ' + e.message;
      }
    },
    async openSession(uuid) {
      this.selectedSession = uuid;
      try {
        const r = await this.apiFetch('/api/sessions/' + encodeURIComponent(uuid));
        if (r.ok) {
          this.sessionDetail = await r.json();
          this.sessionMsg = '';
        } else if (r.status === 404) {
          this.sessionDetail = null;
          this.sessionMsg = 'Session not found (deleted?)';
          await this.loadSessions();
        } else {
          this.sessionMsg = 'Failed to load session: ' + r.status;
        }
      } catch (e) {
        this.sessionMsg = 'Failed to load session: ' + e.message;
      }
    },
    async deleteSession(uuid) {
      if (!confirm('Delete this session permanently? Messages and tool calls will be removed from sessions.db. The live conversation context is NOT affected.')) return;
      try {
        const r = await this.apiFetch('/api/sessions/' + encodeURIComponent(uuid), { method: 'DELETE' });
        if (r.ok) {
          this.sessionDetail = null;
          this.selectedSession = null;
          await this.loadSessions();
        } else {
          this.sessionMsg = 'Delete failed: ' + r.status;
        }
      } catch (e) {
        this.sessionMsg = 'Delete failed: ' + e.message;
      }
    },
    async exportSession(uuid) {
      try {
        const r = await this.apiFetch('/api/sessions/' + encodeURIComponent(uuid) + '/export');
        if (r.ok) {
          const blob = await r.blob();
          const a = document.createElement('a');
          a.href = URL.createObjectURL(blob);
          a.download = 'session-' + uuid + '.json';
          a.click();
          URL.revokeObjectURL(a.href);
        } else {
          this.sessionMsg = 'Export failed: ' + r.status;
        }
      } catch (e) {
        this.sessionMsg = 'Export failed: ' + e.message;
      }
    },
    fmtTime(iso) {
      if (!iso) return '';
      const d = new Date(iso);
      if (isNaN(d.getTime())) return iso;
      return d.toLocaleString();
    },
    async verifyToken() {
      this.authing = true;
      this.authError = '';
      try {
        const r = await fetch('/api/status?token=' + encodeURIComponent(this.token));
        if (r.ok) {
          this.authed = true;
          localStorage.setItem('llaia_token', this.token);
          this.connectWs();
        } else if (r.status === 401) {
          this.authed = false;
          this.authError = 'Incorrect token, please re-enter';
        } else {
          this.authed = false;
          this.authError = 'Service error: ' + r.status;
        }
      } catch (e) {
        this.authed = false;
        this.authError = 'Cannot connect to server: ' + e.message;
      }
      this.authing = false;
    },
    saveToken() {
      this.verifyToken();
    },

    // 回退到登录页：清除内存/本地 token、断开 WS
    forceLogin(reason) {
      if (!this.authed) return; // 已在未登录态，避免重复触发
      this.authed = false;
      this.token = '';
      localStorage.removeItem('llaia_token');
      this.authError = reason || 'Session expired, please re-enter token';
      if (this.ws) { this.ws.onclose = null; this.ws.close(); this.ws = null; }
      if (this._pingTimer) { clearInterval(this._pingTimer); this._pingTimer = null; }
    },
    // 统一的带 token 请求封装：任意 401 视为 token 与 config 不符，回退登录
    async apiFetch(baseUrl, options = {}) {
      const sep = baseUrl.includes('?') ? '&' : '?';
      const url = baseUrl + sep + 'token=' + encodeURIComponent(this.token);
      const r = await fetch(url, options);
      if (r.status === 401) {
        this.forceLogin('Token no longer matches config — please re-enter');
      }
      return r;
    },

    // ---- WS ----
    connectWs() {
      if (this.ws) { this.ws.onclose = null; this.ws.close(); }
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      this.ws = new WebSocket(`${proto}//${location.host}/ws?token=${encodeURIComponent(this.token)}`);
      this.ws.onmessage = (e) => this.onWsMessage(JSON.parse(e.data));
      this.ws.onclose = () => { if (this.authed) setTimeout(() => this.connectWs(), 3000); };
      // 心跳保活：每 25 秒发 ping，防止浏览器/代理关闭空闲 WS
      if (this._pingTimer) clearInterval(this._pingTimer);
      this._pingTimer = setInterval(() => {
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
          this.ws.send(JSON.stringify({ type: 'ping' }));
        }
      }, 25000);
    },
    onWsMessage(ev) {
      switch (ev.type) {
        case 'auth_ok': break;
        case 'auth_failed':
          this.forceLogin('WebSocket authentication failed, check token');
          break;
        case 'chunk':
          if (this.messages.length === 0 || this.messages[this.messages.length-1].role !== 'assistant') {
            this.messages.push({ role: 'assistant', text: ev.delta });
          } else {
            this.messages[this.messages.length-1].text += ev.delta;
          }
          this.scrollBottom();
          break;
        case 'tool_start':
          this.messages.push({ role: 'tool', text: `${ev.name}...` });
          break;
        case 'tool_result':
          this.messages.push({ role: 'tool', text: ev.output });
          break;
        case 'media':
          this.messages.push({ role: 'media', path: ev.path, kind: ev.kind });
          break;
        case 'done':
        case 'error':
        case 'interrupted':
          this.busy = false;
          if (ev.type === 'error') this.messages.push({ role: 'tool', text: `[error: ${ev.message}]` });
          if (ev.type === 'interrupted') this.messages.push({ role: 'tool', text: '[Interrupted]' });
          break;
        case 'busy': alert(ev.reason); break;
        case 'proactive':
          // cron 任务结果等主动推送：插入 chat 流便于用户查看
          this.messages.push({ role: 'tool', text: `[cron] ${ev.message}` });
          this.scrollBottom();
          break;
        case 'pong': break;
      }
    },
    send() {
      if (!this.inputText.trim() && this.uploaded.length === 0) return;
      this.busy = true;
      this.messages.push({ role: 'user', text: this.inputText });
      this.ws.send(JSON.stringify({ type: 'chat', text: this.inputText, images: this.uploaded.map(u=>u.path) }));
      this.inputText = '';
      this.uploaded = [];
      this.scrollBottom();
    },
    stop() { this.ws.send(JSON.stringify({ type: 'stop' })); },
    async onUpload(e) {
      for (const f of e.target.files) {
        const fd = new FormData();
        fd.append('file', f);
        const r = await this.apiFetch('/upload', { method: 'POST', body: fd });
        if (!this.authed) return;
        if (r.ok) { const j = await r.json(); this.uploaded.push(j); }
        else { alert('Upload failed: ' + await r.text()); }
      }
    },
    renderMd(text) {
      try {
        const html = marked.parse(text || '');
        if (window.hljs) {
          // 后处理：对渲染出的代码块做语法高亮（不依赖 marked 版本 API）
          const doc = new DOMParser().parseFromString(html, 'text/html');
          doc.querySelectorAll('pre code').forEach(block => {
            try { window.hljs.highlightElement(block); } catch (_) {}
          });
          return doc.body.innerHTML;
        }
        return html;
      } catch { return text; }
    },
    scrollBottom() { this.$nextTick(() => { const el = this.$refs.messages; if (el) el.scrollTop = el.scrollHeight; }); },

    // ---- config ----
    async switchConfig() {
      this.tab = 'config';
      const r = await this.apiFetch('/api/config');
      if (!this.authed) return;
      console.log('GET /api/config', r.status, r.ok);
      if (r.ok) {
        const data = await r.json();
        console.log('config loaded:', Object.keys(data), 'provider keys:', Object.keys(data.provider || {}));
        // adapt for serde flatten: provider's model is flattened to top level (e.g. qwen3_6),
        // frontend needs to collect non-type/base_url/api_key fields into p.model
        for (const pid in data.provider) {
          const p = data.provider[pid];
          if (!p.model) p.model = {};
          this.compatOpen[pid] = false;
          for (const k of Object.keys(p).slice()) {
            if (k !== 'type' && k !== 'base_url' && k !== 'api_key' && k !== 'model' && k !== 'compat') {
              p.model[k] = p[k];
              delete p[k];
            }
          }
        }
        this.cfg = data;
        // 初始化每个 agent 的 fallback 草稿（下拉选择暂存）
        this.fallbackDraft = {};
        for (const alias in this.cfg.agent) this.fallbackDraft[alias] = '';
        console.log('cfg after transform:', JSON.stringify(this.cfg).slice(0, 200));
      } else {
        console.error('Failed to load config:', r.status, await r.text());
      }
      const rr = await this.apiFetch('/api/config/raw');
      if (!this.authed) return;
      if (rr.ok) { this.rawToml = await rr.text(); this.$nextTick(() => this.initEditor()); }
    },
    initEditor() {
      if (this._editor) { this._editor.setValue(this.rawToml); return; }
      if (this.$refs.rawEditor && window.CodeMirror) {
        this._editor = CodeMirror.fromTextArea(this.$refs.rawEditor, { mode: 'toml', theme: 'material-darker', lineNumbers: true });
        this._editor.setValue(this.rawToml);
      }
    },
    switchRaw() {
      this.configSection = 'raw';
      // 等待 x-if 渲染出 textarea 后再初始化编辑器
      this.$nextTick(() => this.initEditor());
    },
    async saveConfig() {
      if (!confirm('Structured save preserves comments on unchanged sections and applies provider/agent deletions.\nUse the "Raw TOML" editor for full manual control.\n\nContinue?')) return;
      // expand model back to provider top level (adapt for serde flatten)
      // strip NaN (empty input[type=number] yields NaN, JSON.stringify turns it to null, backend u32 parse fails)
      const cfgToSend = JSON.parse(JSON.stringify(this.cfg, (key, value) => {
        // permission: empty/unset → drop so backend stores None (effective default)
        if (key === 'permission' && (value === '' || value === null || value === undefined)) return undefined;
        return typeof value === 'number' && isNaN(value) ? undefined : value;
      }));
      for (const pid in cfgToSend.provider) {
        const p = cfgToSend.provider[pid];
        if (p.model) {
          for (const alias in p.model) { p[alias] = p.model[alias]; }
          delete p.model;
        }
        // 全 null 的 compat 覆盖层等价于未设置，丢弃以免在 TOML 写入空 compat = {}
        if (p.compat && Object.values(p.compat).every(v => v === null)) {
          delete p.compat;
        }
      }
      console.log('PUT /api/config body:', JSON.stringify(cfgToSend).slice(0, 300));
      const r = await this.apiFetch('/api/config', { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify(cfgToSend) });
      if (!this.authed) return;
      let j;
      try { j = await r.json(); } catch { j = {}; }
      console.log('PUT /api/config response:', r.status, j);
      if (r.ok) { alert('Saved ✓\n\n' + (j.note || 'Configuration applied (hot-reloaded).')); this.switchConfig(); } else { alert('Save failed: ' + (j.error||r.status)); }
    },
    // ---- provider/agent CRUD ----
    addProvider() {
      const id = prompt('Enter new provider id (e.g., ollama, openai):');
      if (!id || !id.trim()) return;
      if (this.cfg.provider[id]) { alert('Provider already exists: ' + id); return; }
      this.cfg.provider[id] = { type: 'openai_compatible', base_url: '', api_key: '', model: {} };
      this.compatOpen[id] = false;
    },
    deleteProvider(pid) {
      if (!confirm('Delete provider ' + pid + '? All agents referencing it will break.')) return;
      delete this.cfg.provider[pid];
    },
    // ---- provider compat (provider-level, NOT a model) ----
    toggleCompat(pid) {
      this.compatOpen[pid] = !this.compatOpen[pid];
    },
    enableCompat(pid) {
      // 创建覆盖层对象：所有字段留 null = 继承探测结果（等价于不覆盖）
      this.cfg.provider[pid].compat = {
        supports_developer_role: null,
        reasoning_to_content: null,
        max_tokens_field: null,
        streaming_usage: null,
        infer_finish_reason: null,
        requires_assistant_after_tool: null,
      };
    },
    disableCompat(pid) {
      // 回到 null = 完全不覆盖，纯按 base_url 探测
      this.cfg.provider[pid].compat = null;
    },
    addModel(pid) {
      const alias = prompt('Enter new model alias (e.g., qwen3, gpt4):');
      if (!alias || !alias.trim()) return;
      if (this.cfg.provider[pid].model[alias]) { alert('Model already exists: ' + alias); return; }
      this.cfg.provider[pid].model[alias] = { model: '', native_tool_calling: true, context_size: null };
    },
    deleteModel(pid, alias) {
      if (!confirm('Delete model ' + pid + '.' + alias + '?')) return;
      delete this.cfg.provider[pid].model[alias];
    },

    // ---- 模型探测（P5 W2） ----
    async probeModels(pid) {
      const p = this.cfg.provider[pid];
      this.probing = pid;
      this.probeMsg[pid] = '';
      try {
        const r = await this.apiFetch('/api/providers/' + encodeURIComponent(pid) + '/models', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ base_url: p.base_url, api_key: p.api_key })
        });
        const j = await r.json();
        if (j.ok) {
          this.probeModels[pid] = j.models || [];
          this.probeChecked[pid] = {};
          this.probeMsg[pid] = this.probeModels[pid].length
            ? this.probeModels[pid].length + ' model(s) found'
            : 'Endpoint reachable but no models returned.';
        } else {
          this.probeModels[pid] = [];
          this.probeMsg[pid] = 'Probe failed: ' + (j.error || r.status);
        }
      } catch (e) {
        this.probeModels[pid] = [];
        this.probeMsg[pid] = 'Probe failed: ' + e.message;
      }
      this.probing = null;
    },
    toggleProbeModel(pid, id) {
      if (!this.probeChecked[pid]) this.probeChecked[pid] = {};
      this.probeChecked[pid][id] = !this.probeChecked[pid][id];
    },
    // 生成模型 alias：取 id 尾段 sanitize，冲突时加序号
    genModelAlias(pid, id) {
      let base = id.split(/[/:]/).pop().toLowerCase().replace(/[^a-z0-9_]+/g, '_').replace(/^_+|_+$/g, '');
      if (!base) base = 'model';
      const models = this.cfg.provider[pid].model;
      let alias = base, n = 2;
      while (models[alias]) { alias = base + '_' + n; n++; }
      return alias;
    },
    addProbedModels(pid) {
      const checked = this.probeChecked[pid] || {};
      const picked = (this.probeModels[pid] || []).filter(m => checked[m.id]);
      if (picked.length === 0) { alert('Select at least one model first.'); return; }
      const models = this.cfg.provider[pid].model;
      for (const m of picked) {
        const alias = this.genModelAlias(pid, m.id);
        models[alias] = { model: m.id, native_tool_calling: true, context_size: null };
      }
      this.probeModels[pid] = [];
      this.probeChecked[pid] = {};
      this.probeMsg[pid] = picked.length + ' model(s) added — click Save to persist.';
    },
    addAgent() {
      const alias = prompt('Enter new agent alias (e.g., coder, translator):');
      if (!alias || !alias.trim()) return;
      if (this.cfg.agent[alias]) { alert('Agent already exists: ' + alias); return; }
      this.cfg.agent[alias] = {
        model: '',
        denied_tools: [], delegate_timeout: 120, fallback: [],
      };
      this.fallbackDraft[alias] = '';
    },
    // 所有可选项的 model ref 列表（provider_id.model_alias）
    modelRefs() {
      const refs = [];
      const p = this.cfg.provider || {};
      for (const pid in p) {
        const models = p[pid].model || {};
        for (const m in models) refs.push(pid + '.' + m);
      }
      return refs;
    },
    addFallback(alias) {
      const ref = this.fallbackDraft[alias];
      if (!ref) return;
      const ag = this.cfg.agent[alias];
      if (!ag) return;
      if (!Array.isArray(ag.fallback)) ag.fallback = [];
      if (!ag.fallback.includes(ref)) ag.fallback.push(ref);
      this.fallbackDraft[alias] = '';
    },
    removeFallback(alias, i) {
      const ag = this.cfg.agent[alias];
      if (!ag || !Array.isArray(ag.fallback)) return;
      ag.fallback.splice(i, 1);
    },
    deleteAgent(alias) {
      if (alias === 'main') { alert('Cannot delete main agent'); return; }
      if (!confirm('Delete agent ' + alias + '?')) return;
      delete this.cfg.agent[alias];
    },
    async validateRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await this.apiFetch('/api/config/validate', { method: 'POST', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      if (!this.authed) return;
      const j = await r.json();
      this.rawMsg = j.ok ? '✓ Valid' : '✗ ' + j.error;
    },
    async saveRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await this.apiFetch('/api/config/raw', { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      if (!this.authed) return;
      let j;
      try { j = await r.json(); } catch { j = {}; }
      if (r.ok) { alert('Saved ✓\n\n' + (j.note || 'Configuration applied (hot-reloaded).')); this.switchConfig(); }
      else { alert('Save failed: ' + (j.error || r.status) + (j.line ? '\nPosition (char offset): ' + j.line : '')); }
    },

    // ---- cron ----
    async switchCron() {
      this.tab = 'config';
      this.configSection = 'cron';
      if (this.cronSection === 'tasks') await this.loadCron();
      else if (this.cronSection === 'history') await this.loadCronHistory();
      else if (this.cronSection === 'raw') { await this.loadCronRaw(); this.$nextTick(() => this.initCronEditor()); }
    },
    async switchCronSection(name) {
      this.cronSection = name;
      if (name === 'tasks') await this.loadCron();
      else if (name === 'history') await this.loadCronHistory();
      else if (name === 'raw') { await this.loadCronRaw(); this.$nextTick(() => this.initCronEditor()); }
    },
    async loadCron() {
      this.cronMsg = '';
      const r = await this.apiFetch('/api/cron');
      if (!this.authed) return;
      if (r.ok) { this.cronTasks = await r.json(); }
      else if (r.status === 503) { this.cronTasks = []; this.cronMsg = 'Cron scheduler not running'; }
      else { this.cronMsg = 'Load failed: ' + r.status; }
    },
    async loadCronHistory() {
      const r = await this.apiFetch('/api/cron/history');
      if (!this.authed) return;
      if (r.ok) { this.cronHistory = await r.json(); }
    },
    async loadCronRaw() {
      const r = await this.apiFetch('/api/cron/raw');
      if (!this.authed) return;
      if (r.ok) { const j = await r.json(); this.cronRaw = j.raw || ''; this.cronRawMsg = ''; }
    },
    initCronEditor() {
      if (this._cronEditor) { this._cronEditor.setValue(this.cronRaw); return; }
      if (this.$refs.cronRawEditor && window.CodeMirror) {
        this._cronEditor = CodeMirror.fromTextArea(this.$refs.cronRawEditor, { mode: 'toml', theme: 'material-darker', lineNumbers: true });
        this._cronEditor.setValue(this.cronRaw);
      }
    },
    async saveCronRaw() {
      const toml = this._cronEditor ? this._cronEditor.getValue() : this.cronRaw;
      const r = await this.apiFetch('/api/cron/raw', { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ raw: toml }) });
      if (!this.authed) return;
      let j;
      try { j = await r.json(); } catch { j = {}; }
      if (r.ok) { this.cronRawMsg = '✓ ' + (j.note || 'Saved (hot-reloaded).'); }
      else { this.cronRawMsg = '✗ ' + (j.error || r.status) + (j.line ? ' (char: ' + j.line + ')' : ''); }
    },
    async triggerCron(id) {
      this.cronMsg = `Triggering ${id}...`;
      const r = await this.apiFetch(`/api/cron/${encodeURIComponent(id)}/trigger`, { method: 'POST' });
      if (!this.authed) return;
      if (r.ok) { this.cronMsg = `✓ Task ${id} triggered`; }
      else { let j; try { j = await r.json(); } catch { j = {}; } this.cronMsg = '✗ Trigger failed: ' + (j.error || r.status); }
    },

    // ---- mcp ----
    async switchMcp() {
      this.tab = 'config';
      this.configSection = 'mcp';
      if (this.mcpSection === 'servers') await this.loadMcp();
      else if (this.mcpSection === 'raw') { await this.loadMcpRaw(); this.$nextTick(() => this.initMcpEditor()); }
    },
    async switchMcpSection(name) {
      this.mcpSection = name;
      if (name === 'servers') await this.loadMcp();
      else if (name === 'raw') { await this.loadMcpRaw(); this.$nextTick(() => this.initMcpEditor()); }
    },
    async loadMcp() {
      this.mcpMsg = '';
      const r = await this.apiFetch('/api/mcp');
      if (!this.authed) return;
      if (r.ok) { const j = await r.json(); this.mcpServers = j.servers || []; }
      else if (r.status === 503) { this.mcpServers = []; this.mcpMsg = 'MCP registry not available'; }
      else { this.mcpMsg = 'Load failed: ' + r.status; }
    },
    async loadMcpRaw() {
      const r = await this.apiFetch('/api/mcp/raw');
      if (!this.authed) return;
      if (r.ok) { const j = await r.json(); this.mcpRaw = j.raw || ''; this.mcpRawMsg = ''; }
    },
    initMcpEditor() {
      if (this._mcpEditor) { this._mcpEditor.setValue(this.mcpRaw); return; }
      if (this.$refs.mcpRawEditor && window.CodeMirror) {
        this._mcpEditor = CodeMirror.fromTextArea(this.$refs.mcpRawEditor, { mode: 'toml', theme: 'material-darker', lineNumbers: true });
        this._mcpEditor.setValue(this.mcpRaw);
      }
    },
    async saveMcpRaw() {
      const toml = this._mcpEditor ? this._mcpEditor.getValue() : this.mcpRaw;
      const r = await this.apiFetch('/api/mcp/raw', { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ raw: toml }) });
      if (!this.authed) return;
      let j;
      try { j = await r.json(); } catch { j = {}; }
      if (r.ok) { this.mcpRawMsg = '✓ ' + (j.note || 'Saved (hot-reloaded).'); }
      else { this.mcpRawMsg = '✗ ' + (j.error || r.status); }
    },
    async testMcp(id) {
      this.mcpTesting = id;
      this.mcpMsg = `Testing ${id}...`;
      try {
        const r = await this.apiFetch(`/api/mcp/${encodeURIComponent(id)}/test`, { method: 'POST' });
        if (!this.authed) return;
        let j;
        try { j = await r.json(); } catch { j = {}; }
        if (r.ok && j.ok) {
          this.mcpMsg = `✓ ${id} connected, ${(j.tools || []).length} tools`;
          await this.loadMcp();
        } else {
          this.mcpMsg = `✗ ${id}: ${j.error || r.status}`;
        }
      } finally {
        this.mcpTesting = null;
      }
    },

    // ---- skills ----
    async switchSkills() {
      this.configSection = 'skills';
      this.tab = 'config';
      await this.loadSkills();
    },
    async loadSkills() {
      this.skillMsg = '';
      const r = await this.apiFetch('/api/skills');
      if (!this.authed) return;
      if (r.ok) { const j = await r.json(); this.skills = j.skills || []; }
      else { this.skillMsg = 'Load failed: ' + r.status; }
    },
    async toggleSkill(name, active) {
      const r = await this.apiFetch(`/api/skills/${encodeURIComponent(name)}/active`, { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ active }) });
      if (!this.authed) return;
      if (r.ok) { this.skillMsg = `✓ ${name} ${active ? 'enabled' : 'disabled'}（保存配置或重启后生效）`; }
      else { let j; try { j = await r.json(); } catch { j = {}; } this.skillMsg = '✗ ' + (j.error || r.status); await this.loadSkills(); }
    },
    async editSkill(name) {
      const r = await this.apiFetch(`/api/skills/${encodeURIComponent(name)}/content`);
      if (!this.authed) return;
      if (r.ok) {
        const j = await r.json();
        this.skillEditing = name;
        this.skillContent = j.content || '';
        this.skillContentMsg = '';
      } else { this.skillMsg = 'Load SKILL.md failed: ' + r.status; }
    },
    async saveSkillContent() {
      const name = this.skillEditing;
      if (!name) return;
      const r = await this.apiFetch(`/api/skills/${encodeURIComponent(name)}/content`, { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ content: this.skillContent }) });
      if (!this.authed) return;
      let j;
      try { j = await r.json(); } catch { j = {}; }
      if (r.ok) { this.skillContentMsg = '✓ Saved（保存配置或重启后生效）'; }
      else { this.skillContentMsg = '✗ ' + (j.error || r.status); }
    },
    async newSkill() {
      const name = prompt('Enter new skill name (letters / digits / - _ .):');
      if (!name || !name.trim()) return;
      const r = await this.apiFetch('/api/skills', { method: 'POST', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ name: name.trim() }) });
      if (!this.authed) return;
      let j;
      try { j = await r.json(); } catch { j = {}; }
      if (r.ok) { this.skillMsg = '✓ Created ' + name.trim(); await this.loadSkills(); }
      else { alert('Create failed: ' + (j.error || r.status)); }
    },
    async deleteSkill(name) {
      if (!confirm('Delete skill ' + name + '? The whole skill directory will be removed.')) return;
      const r = await this.apiFetch(`/api/skills/${encodeURIComponent(name)}`, { method: 'DELETE' });
      if (!this.authed) return;
      if (r.ok) { this.skillMsg = '✓ Deleted ' + name; await this.loadSkills(); }
      else { let j; try { j = await r.json(); } catch { j = {}; } this.skillMsg = '✗ ' + (j.error || r.status); }
    },

    // ---- about ----
    async switchAbout() {
      this.tab = 'config';
      this.configSection = 'about';
      const r = await this.apiFetch('/api/status');
      if (!this.authed) return;
      if (r.ok) { this.status = await r.json(); }
    },
    // restart serve process; poll /api/status until the replacement process is up, then reload
    async restartService() {
      if (!confirm('Restart the llaia serve process now?')) return;
      this.restarting = true;
      this.restartMsg = 'Restarting…';
      try {
        const r = await this.apiFetch('/api/restart', { method: 'POST' });
        if (!r.ok) {
          this.restartMsg = 'Restart failed: HTTP ' + r.status;
          this.restarting = false;
          return;
        }
      } catch (e) {
        // old process may have exited before response landed; keep polling
      }
      this.restartMsg = 'Waiting for service to come back…';
      const t0 = Date.now();
      const poll = async () => {
        if (Date.now() - t0 > 30000) {
          this.restartMsg = 'Service did not come back in 30s; check the terminal.';
          this.restarting = false;
          return;
        }
        try {
          const r = await fetch('/api/status?token=' + encodeURIComponent(this.token));
          if (r.ok) { location.reload(); return; }
        } catch (e) { /* still down, retry */ }
        setTimeout(poll, 1500);
      };
      // replacement process starts ~1s after old one exits
      setTimeout(poll, 3000);
    },
    // stop serve process via /api/shutdown; serve_cmd then runs the shared
    // cleanup (cron stop + channel task abort) and exits (ADR-0018)
    async shutdownService() {
      if (!confirm('Stop the llaia serve process now? This will terminate all channels and exit.')) return;
      this.shuttingDown = true;
      this.shutdownMsg = 'Stopping…';
      try {
        const r = await this.apiFetch('/api/shutdown', { method: 'POST' });
        if (!this.authed) return;
        if (r.ok) {
          this.shutdownMsg = 'Service stopped. You may close this page.';
        } else {
          this.shutdownMsg = 'Shutdown failed: HTTP ' + r.status;
          this.shuttingDown = false;
        }
      } catch (e) {
        this.shutdownMsg = 'Request failed: ' + e.message;
        this.shuttingDown = false;
      }
    },
    formatBytes(n) { if (n < 1024) return n + ' B'; if (n < 1048576) return (n/1024).toFixed(1)+' KB'; return (n/1048576).toFixed(1)+' MB'; },
  };
}

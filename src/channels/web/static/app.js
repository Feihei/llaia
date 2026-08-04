function llaiaApp() {
  return {
    tab: 'chat',
    token: localStorage.getItem('llaia_token') || '',
    // 鉴权状态：未校验 / 校验中 / 已通过 / 未通过
    authed: false,
    authing: false,
    authError: '',
    // chat
    messages: [],
    inputText: '',
    busy: false,
    uploaded: [],
    ws: null,
    // config
    cfg: { runtime:{}, log:{}, provider:{}, agent:{}, channels:{cli:{},qq:{},web:{}}, tools:{terminal:{whitelist:[]},tavily:{}} },
    configSection: 'runtime',
    rawToml: '',
    rawMsg: '',
    // about
    status: null,

    async init() {
      // 优先从 URL query 读取 token，其次 localStorage
      const urlParams = new URLSearchParams(location.search);
      const urlToken = urlParams.get('token');
      if (urlToken) {
        this.token = urlToken;
        localStorage.setItem('llaia_token', urlToken);
      }
      // 有 token 就校验，没有直接显示登录界面
      if (this.token) {
        await this.verifyToken();
      }
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
          this.authError = 'Token 不正确，请重新输入';
        } else {
          this.authed = false;
          this.authError = '服务异常: ' + r.status;
        }
      } catch (e) {
        this.authed = false;
        this.authError = '无法连接服务: ' + e.message;
      }
      this.authing = false;
    },
    saveToken() {
      this.verifyToken();
    },

    // ---- WS ----
    connectWs() {
      if (this.ws) this.ws.close();
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      this.ws = new WebSocket(`${proto}//${location.host}/ws?token=${encodeURIComponent(this.token)}`);
      this.ws.onmessage = (e) => this.onWsMessage(JSON.parse(e.data));
      this.ws.onclose = () => { if (this.authed) setTimeout(() => this.connectWs(), 3000); };
    },
    onWsMessage(ev) {
      switch (ev.type) {
        case 'auth_ok': break;
        case 'auth_failed':
          this.authed = false;
          this.authError = 'WebSocket 鉴权失败，请检查 token';
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
          if (ev.type === 'interrupted') this.messages.push({ role: 'tool', text: '[已中断]' });
          break;
        case 'busy': alert(ev.reason); break;
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
        const r = await fetch('/upload?token=' + encodeURIComponent(this.token), { method: 'POST', body: fd });
        if (r.ok) { const j = await r.json(); this.uploaded.push(j); }
        else { alert('上传失败: ' + await r.text()); }
      }
    },
    renderMd(text) { try { return marked.parse(text || ''); } catch { return text; } },
    scrollBottom() { this.$nextTick(() => { const el = this.$refs.messages; if (el) el.scrollTop = el.scrollHeight; }); },

    // ---- config ----
    async switchConfig() {
      this.tab = 'config';
      const r = await fetch('/api/config?token=' + encodeURIComponent(this.token));
      console.log('GET /api/config', r.status, r.ok);
      if (r.ok) {
        const data = await r.json();
        console.log('config loaded:', Object.keys(data), 'provider keys:', Object.keys(data.provider || {}));
        // 适配 serde flatten：provider 的 model 被 flatten 到顶层（如 qwen3_6），
        // 前端需要把除 type/base_url/api_key 以外的字段收集到 p.model
        for (const pid in data.provider) {
          const p = data.provider[pid];
          if (!p.model) p.model = {};
          for (const k of Object.keys(p).slice()) {
            if (k !== 'type' && k !== 'base_url' && k !== 'api_key' && k !== 'model') {
              p.model[k] = p[k];
              delete p[k];
            }
          }
        }
        this.cfg = data;
        console.log('cfg after transform:', JSON.stringify(this.cfg).slice(0, 200));
      } else {
        console.error('Failed to load config:', r.status, await r.text());
      }
      const rr = await fetch('/api/config/raw?token=' + encodeURIComponent(this.token));
      if (rr.ok) { this.rawToml = await rr.text(); this.$nextTick(() => this.initEditor()); }
    },
    initEditor() {
      if (this._editor) { this._editor.setValue(this.rawToml); return; }
      if (this.$refs.rawEditor && window.CodeMirror) {
        this._editor = CodeMirror.fromTextArea(this.$refs.rawEditor, { mode: 'toml', theme: 'material-darker', lineNumbers: true });
        this._editor.setValue(this.rawToml);
      }
    },
    async saveConfig() {
      if (!confirm('结构化保存会重写 config.toml，原始注释和未在 schema 中定义的字段（如 channels.cli.agent）将丢失。\n如需保留注释请使用「原始 TOML」编辑。\n\n确认继续？')) return;
      // 展开 model 回 provider 顶层（适配 serde flatten）
      // 清理 NaN（input[type=number] 空值时 .valueAsNumber 返回 NaN，JSON.stringify 转为 null，后端 u32 反序列化失败）
      const cfgToSend = JSON.parse(JSON.stringify(this.cfg, (key, value) => {
        return typeof value === 'number' && isNaN(value) ? undefined : value;
      }));
      for (const pid in cfgToSend.provider) {
        const p = cfgToSend.provider[pid];
        if (p.model) {
          for (const alias in p.model) { p[alias] = p.model[alias]; }
          delete p.model;
        }
      }
      console.log('PUT /api/config body:', JSON.stringify(cfgToSend).slice(0, 300));
      const r = await fetch('/api/config?token=' + encodeURIComponent(this.token), { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify(cfgToSend) });
      let j;
      try { j = await r.json(); } catch { j = {}; }
      console.log('PUT /api/config response:', r.status, j);
      if (r.ok) { alert('已保存，重启 llaia 生效'); this.switchConfig(); } else { alert('保存失败: ' + (j.error||r.status)); }
    },
    // ---- provider/agent 增删 ----
    addProvider() {
      const id = prompt('输入新 provider id（如 ollama、openai）：');
      if (!id || !id.trim()) return;
      if (this.cfg.provider[id]) { alert('provider 已存在: ' + id); return; }
      this.cfg.provider[id] = { type: 'openai_compatible', base_url: '', api_key: '', model: {} };
    },
    deleteProvider(pid) {
      if (!confirm('确认删除 provider.' + pid + '？所有引用它的 agent 会失效。')) return;
      delete this.cfg.provider[pid];
    },
    addModel(pid) {
      const alias = prompt('输入新 model alias（如 qwen3、gpt4）：');
      if (!alias || !alias.trim()) return;
      if (this.cfg.provider[pid].model[alias]) { alert('model 已存在: ' + alias); return; }
      this.cfg.provider[pid].model[alias] = { model: '', native_tool_calling: true, context_size: null };
    },
    deleteModel(pid, alias) {
      if (!confirm('确认删除 model ' + pid + '.' + alias + '？')) return;
      delete this.cfg.provider[pid].model[alias];
    },
    addAgent() {
      const alias = prompt('输入新 agent alias（如 coder、translator）：');
      if (!alias || !alias.trim()) return;
      if (this.cfg.agent[alias]) { alert('agent 已存在: ' + alias); return; }
      this.cfg.agent[alias] = {
        model: '', workspace: '', soul: null, user: null, memory: null,
        denied_tools: [], delegate_timeout: 120,
      };
    },
    deleteAgent(alias) {
      if (alias === 'main') { alert('main agent 不可删除'); return; }
      if (!confirm('确认删除 agent.' + alias + '？')) return;
      delete this.cfg.agent[alias];
    },
    async validateRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await fetch('/api/config/validate?token=' + encodeURIComponent(this.token), { method: 'POST', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      const j = await r.json();
      this.rawMsg = j.ok ? '✓ 校验通过' : '✗ ' + j.error;
    },
    async saveRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await fetch('/api/config/raw?token=' + encodeURIComponent(this.token), { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      let j;
      try { j = await r.json(); } catch { j = {}; }
      if (r.ok) { alert('已保存，重启 llaia 生效'); this.switchConfig(); }
      else { alert('保存失败: ' + (j.error || r.status) + (j.line ? '\n位置(字符偏移): ' + j.line : '')); }
    },

    // ---- about ----
    async switchAbout() {
      this.tab = 'about';
      const r = await fetch('/api/status?token=' + encodeURIComponent(this.token));
      if (r.ok) { this.status = await r.json(); }
    },
    formatBytes(n) { if (n < 1024) return n + ' B'; if (n < 1048576) return (n/1024).toFixed(1)+' KB'; return (n/1048576).toFixed(1)+' MB'; },
  };
}

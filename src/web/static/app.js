function llaiaApp() {
  return {
    tab: 'chat',
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
    ws: null,
    // config
    cfg: { runtime:{}, log:{}, provider:{}, agent:{}, channels:{cli:{},qq:{},web:{}}, tools:{terminal:{whitelist:[]},tavily:{}} },
    configSection: 'runtime',
    rawToml: '',
    rawMsg: '',
    // about
    status: null,

    async init() {
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
          this.authed = false;
          this.authError = 'WebSocket authentication failed, check token';
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
        else { alert('Upload failed: ' + await r.text()); }
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
        // adapt for serde flatten: provider's model is flattened to top level (e.g. qwen3_6),
        // frontend needs to collect non-type/base_url/api_key fields into p.model
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
    switchRaw() {
      this.configSection = 'raw';
      // 等待 x-if 渲染出 textarea 后再初始化编辑器
      this.$nextTick(() => this.initEditor());
    },
    async saveConfig() {
      if (!confirm('Structured save will rewrite config.toml. Original comments and fields not in the schema (e.g. channels.cli.agent) will be lost.\nTo preserve comments, use the "Raw TOML" editor.\n\nContinue?')) return;
      // expand model back to provider top level (adapt for serde flatten)
      // strip NaN (empty input[type=number] yields NaN, JSON.stringify turns it to null, backend u32 parse fails)
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
      if (r.ok) { alert('Saved, restart llaia to take effect'); this.switchConfig(); } else { alert('Save failed: ' + (j.error||r.status)); }
    },
    // ---- provider/agent CRUD ----
    addProvider() {
      const id = prompt('Enter new provider id (e.g., ollama, openai):');
      if (!id || !id.trim()) return;
      if (this.cfg.provider[id]) { alert('Provider already exists: ' + id); return; }
      this.cfg.provider[id] = { type: 'openai_compatible', base_url: '', api_key: '', model: {} };
    },
    deleteProvider(pid) {
      if (!confirm('Delete provider ' + pid + '? All agents referencing it will break.')) return;
      delete this.cfg.provider[pid];
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
    addAgent() {
      const alias = prompt('Enter new agent alias (e.g., coder, translator):');
      if (!alias || !alias.trim()) return;
      if (this.cfg.agent[alias]) { alert('Agent already exists: ' + alias); return; }
      this.cfg.agent[alias] = {
        model: '', workspace: '', soul: null, user: null, memory: null,
        denied_tools: [], delegate_timeout: 120,
      };
    },
    deleteAgent(alias) {
      if (alias === 'main') { alert('Cannot delete main agent'); return; }
      if (!confirm('Delete agent ' + alias + '?')) return;
      delete this.cfg.agent[alias];
    },
    async validateRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await fetch('/api/config/validate?token=' + encodeURIComponent(this.token), { method: 'POST', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      const j = await r.json();
      this.rawMsg = j.ok ? '✓ Valid' : '✗ ' + j.error;
    },
    async saveRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await fetch('/api/config/raw?token=' + encodeURIComponent(this.token), { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      let j;
      try { j = await r.json(); } catch { j = {}; }
      if (r.ok) { alert('Saved, restart llaia to take effect'); this.switchConfig(); }
      else { alert('Save failed: ' + (j.error || r.status) + (j.line ? '\nPosition (char offset): ' + j.line : '')); }
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

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
    ws: null,
    // config
    cfg: { runtime:{}, log:{}, provider:{}, agent:{}, webui:{}, channels:{qq:{},feishu:{}}, tools:{terminal:{whitelist:[]},tavily:{}} },
    configSection: 'runtime',
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
        // 默认进入 Config 页时，自动拉取配置
        if (this.authed && this.tab === 'config') {
          this.switchConfig();
        }
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
      if (!confirm('Structured save will rewrite config.toml. Original comments and fields not in the schema will be lost.\nTo preserve comments, use the "Raw TOML" editor.\n\nContinue?')) return;
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
      }
      console.log('PUT /api/config body:', JSON.stringify(cfgToSend).slice(0, 300));
      const r = await this.apiFetch('/api/config', { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify(cfgToSend) });
      if (!this.authed) return;
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
      if (r.ok) { alert('Saved, restart llaia to take effect'); this.switchConfig(); }
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
      if (r.ok) { this.cronRawMsg = '✓ Saved (restart serve to apply)'; }
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
      if (r.ok) { this.mcpRawMsg = '✓ Saved (restart serve to apply)'; }
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
      if (r.ok) { this.skillMsg = `✓ ${name} ${active ? 'enabled' : 'disabled'} (restart to apply)`; }
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
      if (r.ok) { this.skillContentMsg = '✓ Saved (restart to apply)'; }
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

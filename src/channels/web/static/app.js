function llaiaApp() {
  return {
    tab: 'chat',
    token: localStorage.getItem('llaia_token') || '',
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
      if (!this.token) { this.token = prompt('请输入 WebUI token:'); if (this.token) localStorage.setItem('llaia_token', this.token); }
      this.connectWs();
    },
    saveToken() { localStorage.setItem('llaia_token', this.token); this.connectWs(); },

    // ---- WS ----
    connectWs() {
      if (this.ws) this.ws.close();
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      this.ws = new WebSocket(`${proto}//${location.host}/ws?token=${encodeURIComponent(this.token)}`);
      this.ws.onmessage = (e) => this.onWsMessage(JSON.parse(e.data));
      this.ws.onclose = () => { setTimeout(() => this.connectWs(), 3000); };
    },
    onWsMessage(ev) {
      switch (ev.type) {
        case 'auth_ok': break;
        case 'auth_failed': alert('token 错误'); break;
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
      if (r.ok) { this.cfg = await r.json(); }
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
      const r = await fetch('/api/config?token=' + encodeURIComponent(this.token), { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify(this.cfg) });
      const j = await r.json();
      if (r.ok) { alert('已保存，重启 llaia 生效'); this.switchConfig(); } else { alert('保存失败: ' + (j.error||r.status)); }
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
      const j = await r.json();
      if (r.ok) { alert('已保存，重启 llaia 生效'); this.switchConfig(); } else { alert('保存失败: ' + (j.error||r.status)); }
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

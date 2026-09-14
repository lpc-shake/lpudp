/* lpudp browser client. No framework or build step is required to run this file. */

const LOCAL_SETTINGS_KEY = "lpudp.client-settings";
const DEFAULT_CLIENT_SETTINGS = {
  window: 90,
  refreshRate: 10,
  channels: 4,
  spectrogram: true,
};
const DEFAULT_ALERT_SETTINGS = {
  enabled: true,
  channel: "EHZ",
  sta_seconds: 6,
  lta_seconds: 30,
  threshold: 3.95,
  reset: 0.9,
  minimum_duration_seconds: 0,
  sound_enabled: true,
  sound_file: "alert.wav",
  screenshot_enabled: true,
};

const state = {
  page: "dashboard",
  clientSettings: loadClientSettings(),
  alertSettings: { ...DEFAULT_ALERT_SETTINGS },
  alertSettingsLoaded: false,
  adminToken: "",
  connection: "disconnected",
  lastError: "",
  station: { status: "Unknown", name: "lpudp station" },
  channels: [],
  waveform: [],
  spectrogram: [],
  packetLoss: null,
  lastPacketAt: null,
  lastAlarm: null,
  serverWindow: 90,
  notice: "",
  socket: null,
};

const app = document.querySelector("#app");
let refreshTimer;

function loadClientSettings() {
  try {
    const saved = JSON.parse(localStorage.getItem(LOCAL_SETTINGS_KEY) || "{}");
    return { ...DEFAULT_CLIENT_SETTINGS, ...saved };
  } catch (_error) {
    return { ...DEFAULT_CLIENT_SETTINGS };
  }
}

function saveClientSettings() {
  localStorage.setItem(LOCAL_SETTINGS_KEY, JSON.stringify(state.clientSettings));
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function formatNumber(value, digits = 1) {
  return Number.isFinite(Number(value)) ? Number(value).toFixed(digits) : "—";
}

function connectionLabel() {
  return { connected: "Connected", connecting: "Connecting…", disconnected: "Disconnected" }[state.connection];
}

function render() {
  app.innerHTML = `
    <header class="topbar">
      <a class="brand" href="#dashboard" data-page="dashboard">lpudp</a>
      <nav aria-label="Primary navigation">
        <a href="#dashboard" data-page="dashboard" class="nav-link ${state.page === "dashboard" ? "active" : ""}">Dashboard</a>
        <a href="#settings" data-page="settings" class="nav-link ${state.page === "settings" ? "active" : ""}">Settings</a>
      </nav>
      <span class="connection-pill ${state.connection}"><i></i>${connectionLabel()}</span>
    </header>
    <main class="content">
      ${state.page === "dashboard" ? dashboardTemplate() : settingsTemplate()}
    </main>`;
  bindEvents();
  if (state.page === "dashboard") {
    drawWaveform(document.querySelector("#waveform"), state.waveform);
    drawSpectrogram(document.querySelector("#spectrogram"), state.spectrogram);
  } else if (!state.alertSettingsLoaded) {
    loadAlertSettings();
  }
}

function dashboardTemplate() {
  const channels = Array.from({ length: state.clientSettings.channels }, (_, index) => state.channels[index] || {});
  const gapCount = state.channels.reduce((total, channel) => total + Number(channel.gap_count || 0), 0);
  const alertText = state.lastAlarm ? `Alarm ${new Date(state.lastAlarm.event_time_ms).toLocaleTimeString()}` : "No recent alarm";
  const windowSeconds = Math.min(state.clientSettings.window, state.serverWindow);
  return `
    <section class="page-heading">
      <div><p class="eyebrow">Live monitor</p><h1>Dashboard</h1><p class="muted">Real-time station telemetry and signal overview.</p></div>
      <button id="connect-button" class="button primary">${state.connection === "connected" ? "Disconnect" : "Connect"}</button>
    </section>
    ${state.lastError ? `<div class="notice error" role="alert">${escapeHtml(state.lastError)}</div>` : state.notice ? `<div class="notice success" role="status">${escapeHtml(state.notice)}</div>` : ""}
    <section class="summary-grid" aria-label="Station summary">
      <article class="status-card"><span class="label">Station status</span><strong class="status-${String(state.station.status).toLowerCase()}">${escapeHtml(state.station.status)}</strong><span class="muted">${escapeHtml(state.station.name)}</span></article>
      <article class="status-card"><span class="label">Connection</span><strong>${connectionLabel()}</strong><span class="muted">${state.lastPacketAt ? `Last packet ${new Date(state.lastPacketAt).toLocaleTimeString()}` : "Waiting for live data"}</span></article>
      <article class="status-card"><span class="label">Data gaps</span><strong class="${gapCount ? "bad" : "good"}">${gapCount}</strong><span class="muted">Detected between UDP blocks</span></article>
      <article class="status-card"><span class="label">Alerts</span><strong class="${state.lastAlarm ? "bad" : state.alertSettings.enabled ? "good" : "muted-text"}">${state.lastAlarm ? "Alarm" : state.alertSettings.enabled ? "Enabled" : "Disabled"}</strong><span class="muted">${alertText}</span></article>
    </section>
    <section class="panel"><div class="panel-heading"><div><h2>Channels</h2><p class="muted">${state.channels.length || 0} channels reporting</p></div></div><div class="channel-grid">${channels.map(channelTemplate).join("")}</div></section>
    <section class="visual-grid">
      <article class="panel visual-panel"><div class="panel-heading"><h2>Waveform</h2><span class="muted">${windowSeconds}s window</span></div><canvas id="waveform" aria-label="Waveform visualization"></canvas></article>
      <article class="panel visual-panel"><div class="panel-heading"><h2>Spectrogram</h2><span class="muted">${state.clientSettings.spectrogram ? "Live spectrum" : "Disabled"}</span></div><canvas id="spectrogram" aria-label="Spectrogram visualization"></canvas></article>
    </section>`;
}

function channelTemplate(channel, index) {
  const level = channel.level ?? channel.rms ?? channel.db;
  const peak = channel.peak ?? channel.peakDb;
  const name = channel.name || channel.label || `Channel ${index + 1}`;
  const meter = channel.meter ?? Math.max(0, Math.min(100, Number(level) || 0));
  return `<article class="channel-card"><div class="channel-title"><span class="channel-dot"></span><h3>${escapeHtml(name)}</h3><span class="channel-state">${escapeHtml(channel.status || "Active")}</span></div><div class="meter"><span style="width:${meter}%"></span></div><div class="channel-values"><span>Level <b>${formatNumber(level)} dB</b></span><span>Peak <b>${formatNumber(peak)} dB</b></span></div></article>`;
}

function settingsTemplate() {
  return `<section class="page-heading"><div><p class="eyebrow">Preferences</p><h1>Settings</h1><p class="muted">Client display options are stored in this browser. Alert policy is shared by the station.</p></div></section>
    ${state.lastError ? `<div class="notice error" role="alert">${escapeHtml(state.lastError)}</div>` : state.notice ? `<div class="notice success" role="status">${escapeHtml(state.notice)}</div>` : ""}
    <form id="settings-form" class="settings-layout">
      <section class="panel settings-panel"><h2>Display settings</h2><p class="muted">These values affect this browser only.</p>
        <label>Window (seconds)<input name="window" type="number" min="1" max="90" step="1" value="${state.clientSettings.window}" /></label>
        <label>Refresh rate (Hz)<input name="refreshRate" type="number" min="1" max="60" step="1" value="${state.clientSettings.refreshRate}" /></label>
        <label>Channels<select name="channels"><option value="2" ${state.clientSettings.channels === 2 ? "selected" : ""}>2</option><option value="4" ${state.clientSettings.channels === 4 ? "selected" : ""}>4</option><option value="8" ${state.clientSettings.channels === 8 ? "selected" : ""}>8</option></select></label>
        <label class="check-row"><input name="spectrogram" type="checkbox" ${state.clientSettings.spectrogram ? "checked" : ""} /> Show spectrogram</label>
        <button class="button primary" type="submit">Save display settings</button>
      </section>
      <section class="panel settings-panel"><h2>Shared alert settings</h2><p class="muted">Read from and saved to the station API.</p>
        <label>Admin token<input name="adminToken" type="password" autocomplete="off" placeholder="Required to save" value="${escapeHtml(state.adminToken)}" /></label>
        <label class="check-row"><input name="alertsEnabled" type="checkbox" ${state.alertSettings.enabled ? "checked" : ""} /> Enable alerts</label>
        <label>Alert channel<select name="alertChannel"><option value="EHZ" ${state.alertSettings.channel === "EHZ" ? "selected" : ""}>EHZ</option><option value="EHE" ${state.alertSettings.channel === "EHE" ? "selected" : ""}>EHE</option><option value="EHN" ${state.alertSettings.channel === "EHN" ? "selected" : ""}>EHN</option><option value="ENZ" ${state.alertSettings.channel === "ENZ" ? "selected" : ""}>ENZ</option></select></label>
        <div class="field-grid"><label>STA (seconds)<input name="staSeconds" type="number" min="0.01" step="0.01" value="${state.alertSettings.sta_seconds}" /></label><label>LTA (seconds)<input name="ltaSeconds" type="number" min="0.02" step="0.01" value="${state.alertSettings.lta_seconds}" /></label></div>
        <div class="field-grid"><label>Trigger ratio<input name="threshold" type="number" min="0.01" step="0.01" value="${state.alertSettings.threshold}" /></label><label>Reset ratio<input name="reset" type="number" min="0" step="0.01" value="${state.alertSettings.reset}" /></label></div>
        <label>Minimum duration (seconds)<input name="minimumDuration" type="number" min="0" step="0.01" value="${state.alertSettings.minimum_duration_seconds}" /></label>
        <label class="check-row"><input name="soundEnabled" type="checkbox" ${state.alertSettings.sound_enabled ? "checked" : ""} /> Play alert sound</label>
        <label>Sound file path<input name="soundFile" type="text" value="${escapeHtml(state.alertSettings.sound_file)}" /></label>
        <label class="check-row"><input name="screenshotEnabled" type="checkbox" ${state.alertSettings.screenshot_enabled ? "checked" : ""} /> Save graph image on alarm</label>
        <div class="button-row"><button id="load-alerts" class="button" type="button">Load from station</button><button class="button primary" type="submit" name="saveAlerts" value="true">Save shared alerts</button></div>
      </section>
    </form>`;
}

function bindEvents() {
  document.querySelectorAll("[data-page]").forEach((link) => link.addEventListener("click", (event) => {
    event.preventDefault(); state.page = link.dataset.page; state.lastError = ""; render();
  }));
  document.querySelector("#connect-button")?.addEventListener("click", () => state.connection === "connected" ? disconnect() : connect());
  document.querySelector("#settings-form")?.addEventListener("submit", handleSettingsSubmit);
  document.querySelector("#load-alerts")?.addEventListener("click", () => loadAlertSettings(true));
}

function wsUrl() {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}/api/v1/live`;
}

function connect() {
  if (state.socket) state.socket.close();
  state.connection = "connecting"; state.lastError = ""; render();
  try {
    const socket = new WebSocket(wsUrl()); state.socket = socket;
    socket.addEventListener("open", () => { state.connection = "connected"; state.lastError = ""; render(); });
    socket.addEventListener("message", (event) => handleLiveMessage(event.data));
    socket.addEventListener("error", () => { state.lastError = "Live connection failed. Check the server and try again."; });
    socket.addEventListener("close", () => { state.connection = "disconnected"; state.socket = null; render(); });
  } catch (error) { state.connection = "disconnected"; state.lastError = `Could not open live connection: ${error.message}`; render(); }
}

function disconnect() { state.socket?.close(); state.socket = null; state.connection = "disconnected"; render(); }

function handleLiveMessage(raw) {
  try {
    const message = typeof raw === "string" ? JSON.parse(raw) : raw;
    if (message.type === "Alarm") {
      state.lastAlarm = message;
      state.notice = `Possible event on ${message.channel || "the station"}.`;
      render();
      if (state.alertSettings.screenshot_enabled) setTimeout(saveGraphImages, 0);
      return;
    }
    if (message.type === "Reset") {
      state.notice = "Alert condition cleared.";
      render();
      return;
    }
    const data = message.data || message;
    if (data.status) {
      const status = data.status;
      state.station = { status: status.alert_active ? "Alert" : "Online", name: status.demo ? "Demo source" : (status.source_ip || "Raspberry Shake") };
      if (!status.alert_active) state.lastAlarm = null;
    }
    if (Number.isFinite(Number(data.display_window_seconds))) state.serverWindow = Number(data.display_window_seconds);
    if (Array.isArray(data.channels)) {
      state.channels = data.channels.map(channelMetrics);
      const primary = state.channels.find((channel) => channel.channel === state.alertSettings.channel) || state.channels[0];
      const allSamples = primary?.samples || [];
      const visibleCount = Math.max(1, Math.floor(allSamples.length * Math.min(1, state.clientSettings.window / Math.max(1, state.serverWindow))));
      state.waveform = allSamples.slice(-visibleCount);
      state.spectrogram = state.clientSettings.spectrogram ? buildSpectrogram(state.waveform) : [];
    }
    if (Array.isArray(data.spectrogram)) state.spectrogram = data.spectrogram;
    state.lastPacketAt = Date.now(); state.lastError = "";
  } catch (_error) { state.lastError = "Received an unreadable live packet."; render(); }
}

function channelMetrics(channel) {
  const samples = Array.isArray(channel.samples) ? channel.samples.map(Number) : [];
  if (!samples.length) return { ...channel, name: channel.channel, level: -120, peak: -120, meter: 0, status: "Waiting" };
  const sum = samples.reduce((total, value) => total + value * value, 0);
  const rms = Math.sqrt(sum / samples.length);
  const peak = Math.max(...samples.map((value) => Math.abs(value)));
  const level = rms > 0 ? 20 * Math.log10(rms) : -120;
  const peakDb = peak > 0 ? 20 * Math.log10(peak) : -120;
  return { ...channel, name: channel.channel, level, peak: peakDb, meter: Math.max(0, Math.min(100, (level + 120) / 120 * 100)), status: "Active" };
}

async function apiRequest(path, options = {}) {
  const headers = { Accept: "application/json", ...(options.body ? { "Content-Type": "application/json" } : {}), ...(options.headers || {}) };
  const response = await fetch(path, { ...options, headers });
  if (!response.ok) throw new Error(`Station API returned ${response.status} ${response.statusText}`);
  return response.status === 204 ? null : response.json();
}

async function loadAlertSettings(force = false) {
  if (state.alertSettingsLoaded && !force) return;
  state.alertSettingsLoaded = true;
  state.lastError = ""; render();
  try { const settings = await apiRequest("/api/v1/settings"); state.alertSettings = { ...DEFAULT_ALERT_SETTINGS, ...(settings.alert || {}) }; render(); }
  catch (error) { state.lastError = `Could not load shared settings: ${error.message}`; render(); }
}

async function handleSettingsSubmit(event) {
  event.preventDefault(); const form = new FormData(event.currentTarget);
  state.clientSettings = { window: Number(form.get("window")), refreshRate: Number(form.get("refreshRate")), channels: Number(form.get("channels")), spectrogram: form.get("spectrogram") === "on" };
  saveClientSettings();
  restartRefreshTimer();
  if (form.get("saveAlerts") !== "true") { state.notice = "Display settings saved in this browser."; state.lastError = ""; render(); return; }
  state.adminToken = String(form.get("adminToken") || "");
  try {
    const alert = { enabled: form.get("alertsEnabled") === "on", channel: String(form.get("alertChannel")), sta_seconds: Number(form.get("staSeconds")), lta_seconds: Number(form.get("ltaSeconds")), threshold: Number(form.get("threshold")), reset: Number(form.get("reset")), minimum_duration_seconds: Number(form.get("minimumDuration")), sound_enabled: form.get("soundEnabled") === "on", sound_file: String(form.get("soundFile") || ""), screenshot_enabled: form.get("screenshotEnabled") === "on" };
    const result = await apiRequest("/api/v1/settings", { method: "PUT", headers: { "X-Admin-Token": state.adminToken }, body: JSON.stringify({ alert }) });
    state.alertSettings = { ...DEFAULT_ALERT_SETTINGS, ...(result.alert || alert) }; state.notice = "Shared alert settings saved."; state.lastError = ""; render();
  } catch (error) { state.lastError = `Could not save shared settings: ${error.message}`; render(); }
}

function drawWaveform(canvas, samples) {
  if (!canvas) return; const ctx = canvas.getContext("2d"); const width = canvas.width = canvas.clientWidth * devicePixelRatio; const height = canvas.height = canvas.clientHeight * devicePixelRatio;
  ctx.clearRect(0, 0, width, height);
  if (!samples.length) { ctx.fillStyle = "#9aabc0"; ctx.font = `${14 * devicePixelRatio}px system-ui`; ctx.fillText("Waiting for data", 18 * devicePixelRatio, height / 2); return; }
  ctx.strokeStyle = "#48d597"; ctx.lineWidth = 2 * devicePixelRatio; ctx.beginPath();
  const scale = Math.max(1, ...samples.map((value) => Math.abs(Number(value))));
  const values = samples.map((value) => Number(value) / scale);
  values.forEach((value, i) => { const x = i / Math.max(1, values.length - 1) * width; const y = height / 2 - Number(value) * height * 0.38; i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); }); ctx.stroke();
}

function buildSpectrogram(samples) {
  if (!samples.length) return [];
  const rows = 36;
  const bins = 48;
  const size = 128;
  const step = Math.max(1, Math.floor((samples.length - size) / Math.max(1, rows - 1)));
  const result = [];
  const scale = Math.max(1, ...samples.map((value) => Math.abs(Number(value))));
  for (let row = 0; row < rows; row += 1) {
    const offset = Math.min(row * step, Math.max(0, samples.length - size));
    const values = [];
    for (let bin = 0; bin < bins; bin += 1) {
      let real = 0; let imaginary = 0;
      for (let index = 0; index < size; index += 1) {
        const window = 0.5 - 0.5 * Math.cos(2 * Math.PI * index / (size - 1));
        const angle = 2 * Math.PI * bin * index / size;
        const value = Number(samples[offset + index] || 0) / scale * window;
        real += value * Math.cos(angle); imaginary -= value * Math.sin(angle);
      }
      values.push(Math.log1p(Math.sqrt(real * real + imaginary * imaginary)));
    }
    result.push(values);
  }
  return result;
}

function drawSpectrogram(canvas, rows) {
  if (!canvas) return; const ctx = canvas.getContext("2d"); const width = canvas.width = canvas.clientWidth * devicePixelRatio; const height = canvas.height = canvas.clientHeight * devicePixelRatio; ctx.fillStyle = "#101c2b"; ctx.fillRect(0, 0, width, height);
  if (!rows.length) { ctx.fillStyle = "#9aabc0"; ctx.font = `${14 * devicePixelRatio}px system-ui`; ctx.fillText("Waiting for spectrum", 18 * devicePixelRatio, height / 2); return; }
  const image = ctx.createImageData(width, height); for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) { const seed = Number(rows[y * rows.length / height | 0]?.[x * (rows[0]?.length || 1) / width | 0] || 0); const intensity = Math.max(0, Math.min(255, 55 + seed * 100 + (x / width) * 45)); const p = (y * width + x) * 4; image.data[p] = intensity * 0.35; image.data[p + 1] = intensity * 0.72; image.data[p + 2] = intensity; image.data[p + 3] = 255; } ctx.putImageData(image, 0, 0);
}

function saveGraphImages() {
  const waveform = document.querySelector("#waveform");
  const spectrogram = document.querySelector("#spectrogram");
  if (!waveform || !spectrogram) return;
  const output = document.createElement("canvas");
  output.width = waveform.width + spectrogram.width;
  output.height = Math.max(waveform.height, spectrogram.height);
  const context = output.getContext("2d");
  context.fillStyle = "#0b1220"; context.fillRect(0, 0, output.width, output.height);
  context.drawImage(waveform, 0, 0); context.drawImage(spectrogram, waveform.width, 0);
  const link = document.createElement("a");
  link.download = `lpudp-alarm-${new Date().toISOString().replaceAll(":", "-")}.png`;
  link.href = output.toDataURL("image/png");
  link.click();
}

window.addEventListener("resize", () => { if (state.page === "dashboard") { drawWaveform(document.querySelector("#waveform"), state.waveform); drawSpectrogram(document.querySelector("#spectrogram"), state.spectrogram); } });
function restartRefreshTimer() {
  clearInterval(refreshTimer);
  refreshTimer = setInterval(() => { if (state.page === "dashboard" && state.connection === "connected") render(); }, 1000 / Math.max(1, state.clientSettings.refreshRate));
}
restartRefreshTimer();
window.addEventListener("hashchange", () => { const page = location.hash.slice(1); if (["dashboard", "settings"].includes(page)) { state.page = page; render(); } });
state.page = location.hash.slice(1) === "settings" ? "settings" : "dashboard";
render();
connect();
loadAlertSettings();

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const inputDeviceSelect = document.getElementById(
  "input-device-select",
) as HTMLSelectElement;
const outputDeviceSelect = document.getElementById(
  "output-device-select",
) as HTMLSelectElement;
const midiInSelect = document.getElementById(
  "midi-in-select",
) as HTMLSelectElement;
const midiOutSelect = document.getElementById(
  "midi-out-select",
) as HTMLSelectElement;
const profileSelect = document.getElementById(
  "profile-select",
) as HTMLSelectElement;
const profileHint = document.getElementById("profile-hint") as HTMLElement;
const simulateCheck = document.getElementById(
  "simulate-check",
) as HTMLInputElement;

const refreshBtn = document.getElementById(
  "refresh-devices-btn",
) as HTMLButtonElement;
const saveSettingsBtn = document.getElementById(
  "save-settings-btn",
) as HTMLButtonElement;
const selfTestBtn = document.getElementById(
  "self-test-btn",
) as HTMLButtonElement;

const startListenBtn = document.getElementById(
  "start-listening-btn",
) as HTMLButtonElement;
const stopListenBtn = document.getElementById(
  "stop-listening-btn",
) as HTMLButtonElement;
const startEncodeBtn = document.getElementById(
  "start-encode-btn",
) as HTMLButtonElement;
const stopEncodeBtn = document.getElementById(
  "stop-encode-btn",
) as HTMLButtonElement;

const decodedDisplay = document.getElementById(
  "decoded-display",
) as HTMLDivElement;
const inputMeter = document.getElementById("input-meter") as HTMLDivElement;
const logContainer = document.getElementById("log-container") as HTMLDivElement;
const alarmBox = document.getElementById("alarm-box") as HTMLDivElement;
const decodeCard = document.getElementById("decode-card") as HTMLElement;
const encodeCard = document.getElementById("encode-card") as HTMLElement;

const linkStateEl = document.getElementById("link-state") as HTMLElement;
const linkAliveEl = document.getElementById("link-alive") as HTMLElement;
const linkConfidenceEl = document.getElementById(
  "link-confidence",
) as HTMLElement;
const linkLatencyEl = document.getElementById("link-latency") as HTMLElement;
const linkCrcEl = document.getElementById("link-crc") as HTMLElement;
const linkCrcRateEl = document.getElementById("link-crc-rate") as HTMLElement;
const linkSyncLostEl = document.getElementById(
  "link-sync-lost",
) as HTMLElement;
const linkDropsEl = document.getElementById("link-drops") as HTMLElement;
const linkSeqEl = document.getElementById("link-seq") as HTMLElement;
const linkLastEventEl = document.getElementById(
  "link-last-event",
) as HTMLElement;
const confidenceMeter = document.getElementById(
  "confidence-meter",
) as HTMLDivElement;

interface AppSettings {
  profile: string;
  mode: string;
  audio_in: number | null;
  audio_out: number | null;
  midi_in: number | null;
  midi_out: number | null;
}

function log(msg: string) {
  const p = document.createElement("div");
  p.textContent = `[${new Date().toLocaleTimeString()}] ${msg}`;
  logContainer.appendChild(p);
  logContainer.scrollTop = logContainer.scrollHeight;
}

/** Kind of the alarm the banner currently shows (null when hidden). */
let alarmKind: string | null = null;

function showAlarm(kind: string, message: string) {
  alarmKind = kind;
  alarmBox.textContent = message;
  alarmBox.classList.add("visible");
}

function clearAlarm() {
  alarmKind = null;
  alarmBox.classList.remove("visible");
}

function selectedIndex(select: HTMLSelectElement): number | null {
  const v = parseInt(select.value, 10);
  return Number.isNaN(v) ? null : v;
}

function populateSelect(
  select: HTMLSelectElement,
  devices: string[],
  preferred?: number | null,
) {
  select.innerHTML = "";
  if (devices.length === 0) {
    const opt = document.createElement("option");
    opt.text = "No devices found";
    opt.value = "";
    select.add(opt);
    return;
  }
  devices.forEach((name, index) => {
    const opt = document.createElement("option");
    opt.value = String(index);
    opt.text = name;
    select.add(opt);
  });
  if (preferred != null && preferred >= 0 && preferred < devices.length) {
    select.selectedIndex = preferred;
  }
}

function currentMode(): string {
  const el = document.querySelector(
    'input[name="mode"]:checked',
  ) as HTMLInputElement | null;
  return el?.value ?? "both";
}

function applyModeVisibility() {
  const mode = currentMode();
  decodeCard.style.display = mode === "encode" ? "none" : "";
  encodeCard.style.display = mode === "decode" ? "none" : "";
}

async function refreshProfileHint() {
  try {
    const info = await invoke<{ est_latency_ms: number }>("get_profile_info", {
      profile: profileSelect.value,
    });
    linkLatencyEl.textContent = info.est_latency_ms.toFixed(0);
    profileHint.textContent =
      profileSelect.value === "fast"
        ? `Fast ≈ ${info.est_latency_ms.toFixed(0)} ms — clean dedicated chain only.`
        : `Robust ≈ ${info.est_latency_ms.toFixed(0)} ms — preferred default.`;
  } catch (e) {
    log(`Profile info error: ${e}`);
  }
}

async function loadDevices() {
  try {
    log("Loading devices...");
    const [audioIn, audioOut, midiIn, midiOut, settings] = await Promise.all([
      invoke<string[]>("get_input_devices"),
      invoke<string[]>("get_output_devices"),
      invoke<string[]>("get_midi_inputs"),
      invoke<string[]>("get_midi_outputs"),
      invoke<AppSettings>("get_settings"),
    ]);

    populateSelect(inputDeviceSelect, audioIn, settings.audio_in);
    populateSelect(outputDeviceSelect, audioOut, settings.audio_out);
    populateSelect(midiInSelect, midiIn, settings.midi_in);
    populateSelect(midiOutSelect, midiOut, settings.midi_out);

    profileSelect.value = settings.profile || "robust";
    const modeRadio = document.querySelector(
      `input[name="mode"][value="${settings.mode || "both"}"]`,
    ) as HTMLInputElement | null;
    if (modeRadio) modeRadio.checked = true;
    applyModeVisibility();
    await refreshProfileHint();

    log(
      `Audio in/out ${audioIn.length}/${audioOut.length}, MIDI in/out ${midiIn.length}/${midiOut.length}`,
    );
  } catch (error) {
    log(`Error loading devices: ${error}`);
  }
}

async function persistSettings() {
  const settings: AppSettings = {
    profile: profileSelect.value,
    mode: currentMode(),
    audio_in: selectedIndex(inputDeviceSelect),
    audio_out: selectedIndex(outputDeviceSelect),
    midi_in: selectedIndex(midiInSelect),
    midi_out: selectedIndex(midiOutSelect),
  };
  await invoke("set_settings", { settings });
  log("Settings saved.");
}

refreshBtn.addEventListener("click", loadDevices);
saveSettingsBtn.addEventListener("click", () => {
  persistSettings().catch((e) => log(`Save failed: ${e}`));
});
profileSelect.addEventListener("change", () => {
  refreshProfileHint();
});
document.querySelectorAll('input[name="mode"]').forEach((el) => {
  el.addEventListener("change", applyModeVisibility);
});

selfTestBtn.addEventListener("click", async () => {
  try {
    const result = await invoke<string>("run_self_test", {
      profile: profileSelect.value,
    });
    log(result);
  } catch (e) {
    log(`Self-test failed: ${e}`);
  }
});

startListenBtn.addEventListener("click", async () => {
  try {
    await invoke("start_decoder", {
      audioIn: selectedIndex(inputDeviceSelect),
      midiOut: selectedIndex(midiOutSelect),
      profile: profileSelect.value,
    });
    startListenBtn.disabled = true;
    stopListenBtn.disabled = false;
    inputDeviceSelect.disabled = true;
    midiOutSelect.disabled = true;
    decodedDisplay.innerHTML = '<span style="color: #3d9a7a">Listening…</span>';
    clearAlarm();
    await persistSettings();
  } catch (error) {
    log(`Error starting decoder: ${error}`);
  }
});

stopListenBtn.addEventListener("click", async () => {
  try {
    await invoke("stop_decoder");
    startListenBtn.disabled = false;
    stopListenBtn.disabled = true;
    inputDeviceSelect.disabled = false;
    midiOutSelect.disabled = false;
    decodedDisplay.innerHTML = '<span style="color: #555">Stopped</span>';
    log("Decoder stopped.");
  } catch (error) {
    log(`Error stopping decoder: ${error}`);
  }
});

startEncodeBtn.addEventListener("click", async () => {
  try {
    const midiIn = selectedIndex(midiInSelect);
    await invoke("start_encoder", {
      audioOut: selectedIndex(outputDeviceSelect),
      midiIn,
      profile: profileSelect.value,
      simulate: simulateCheck.checked || midiIn === null,
    });
    startEncodeBtn.disabled = true;
    stopEncodeBtn.disabled = false;
    outputDeviceSelect.disabled = true;
    midiInSelect.disabled = true;
    await persistSettings();
  } catch (error) {
    log(`Error starting encoder: ${error}`);
  }
});

stopEncodeBtn.addEventListener("click", async () => {
  try {
    await invoke("stop_encoder");
    startEncodeBtn.disabled = false;
    stopEncodeBtn.disabled = true;
    outputDeviceSelect.disabled = false;
    midiInSelect.disabled = false;
    log("Encoder stopped.");
  } catch (error) {
    log(`Error stopping encoder: ${error}`);
  }
});

async function bindEvents() {
  await listen<{ message: string }>("log", (event) => {
    log(event.payload.message);
  });

  await listen<{
    frame_type: string;
    payload: string;
    is_heartbeat: boolean;
  }>("decoded_frame", (event) => {
    const { frame_type, payload, is_heartbeat } = event.payload;
    if (is_heartbeat) {
      linkAliveEl.textContent = "yes";
      linkAliveEl.style.color = "#3d9a7a";
      return;
    }
    const text = payload ? `${frame_type}: ${payload}` : frame_type;
    decodedDisplay.textContent = text;
    linkLastEventEl.textContent = text;
    decodedDisplay.style.backgroundColor = "#1a2220";
    setTimeout(() => (decodedDisplay.style.backgroundColor = "#0d0f12"), 100);
  });

  await listen<number>("input_level", (event) => {
    const percent = Math.min(event.payload * 400, 100);
    inputMeter.style.width = `${percent}%`;
    inputMeter.style.backgroundColor = percent > 90 ? "#d64545" : "#3d9a7a";
  });

  await listen<{
    state: string;
    crc_pass: number;
    crc_fail: number;
    confidence_db: number;
    last_seq: number | null;
    sync_lost: number;
    crc_pass_rate: number;
    est_latency_ms: number;
    link_alive: boolean;
    buffer_drops: number;
  }>("link_stats", (event) => {
    const s = event.payload;
    linkStateEl.textContent = s.state;
    linkConfidenceEl.textContent = s.confidence_db.toFixed(1);
    linkCrcEl.textContent = `${s.crc_pass} / ${s.crc_fail}`;
    linkCrcRateEl.textContent = s.crc_pass_rate.toFixed(0);
    linkSyncLostEl.textContent = String(s.sync_lost);
    linkDropsEl.textContent = String(s.buffer_drops);
    linkSeqEl.textContent = s.last_seq === null ? "—" : String(s.last_seq);
    linkLatencyEl.textContent = s.est_latency_ms.toFixed(0);
    linkAliveEl.textContent = s.link_alive ? "yes" : "no";
    linkAliveEl.style.color = s.link_alive ? "#3d9a7a" : "#9aa3ad";
    confidenceMeter.style.width = `${Math.min(Math.max(s.confidence_db, 0) * 10, 100)}%`;
    if (s.state === "Locked") linkStateEl.style.color = "#3d9a7a";
    else if (s.state === "Dead") linkStateEl.style.color = "#d64545";
    else linkStateEl.style.color = "#d4a017";
  });

  await listen<{ kind: string; message: string }>("alarm", (event) => {
    showAlarm(event.payload.kind, event.payload.message);
    log(`ALARM: ${event.payload.message}`);
  });

  // The engine reports when an alarm's condition has ended (e.g. audio input
  // resumed after a stall): drop the banner if it still shows that alarm.
  await listen<{ kind: string; message: string }>("alarm_cleared", (event) => {
    if (alarmKind === event.payload.kind) clearAlarm();
    log(event.payload.message);
  });
}

bindEvents()
  .then(() => loadDevices())
  .catch((e) => console.error(e));

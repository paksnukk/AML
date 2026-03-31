import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

let inputDevices: string[] = [];
let outputDevices: string[] = [];

// DOM Elements
const inputDeviceSelect = document.getElementById(
  "input-device-select",
) as HTMLSelectElement;
const outputDeviceSelect = document.getElementById(
  "output-device-select",
) as HTMLSelectElement;
const refreshBtn = document.getElementById(
  "refresh-devices-btn",
) as HTMLButtonElement;

const startListenBtn = document.getElementById(
  "start-listening-btn",
) as HTMLButtonElement;
const stopListenBtn = document.getElementById(
  "stop-listening-btn",
) as HTMLButtonElement;
const decodedDisplay = document.getElementById(
  "decoded-display",
) as HTMLDivElement;

const inputMeter = document.getElementById("input-meter") as HTMLDivElement;

const startTestBtn = document.getElementById(
  "start-test-btn",
) as HTMLButtonElement;
const stopTestBtn = document.getElementById(
  "stop-test-btn",
) as HTMLButtonElement;

const logContainer = document.getElementById("log-container") as HTMLDivElement;

// Helper to log to UI
function log(msg: string) {
  const p = document.createElement("div");
  p.textContent = `[${new Date().toLocaleTimeString()}] ${msg}`;
  logContainer.appendChild(p);
  logContainer.scrollTop = logContainer.scrollHeight;
}

// Fetch Devices
async function loadDevices() {
  try {
    log("Loading devices...");
    inputDevices = await invoke("get_input_devices");
    outputDevices = await invoke("get_output_devices");

    populateSelect(inputDeviceSelect, inputDevices);
    populateSelect(outputDeviceSelect, outputDevices);
    log(
      `Found ${inputDevices.length} input and ${outputDevices.length} output devices.`,
    );
  } catch (error) {
    log(`Error loading devices: ${error}`);
  }
}

function populateSelect(select: HTMLSelectElement, devices: string[]) {
  select.innerHTML = "";
  if (devices.length === 0) {
    const opt = document.createElement("option");
    opt.text = "No devices found";
    select.add(opt);
    return;
  }

  devices.forEach((name, index) => {
    const opt = document.createElement("option");
    opt.value = index.toString();
    opt.text = name;
    select.add(opt);
  });

  // Select default if available
  if (devices.length > 0) {
    select.selectedIndex = 0;
  }
}

// Event Listeners
refreshBtn.addEventListener("click", loadDevices);

startListenBtn.addEventListener("click", async () => {
  const index = parseInt(inputDeviceSelect.value);
  if (isNaN(index)) {
    log("Please select an input device.");
    return;
  }

  try {
    await invoke("start_listening", { deviceIndex: index });
    startListenBtn.disabled = true;
    stopListenBtn.disabled = false;
    inputDeviceSelect.disabled = true;
    decodedDisplay.innerHTML =
      '<span style="color: #0f0; animation: blink 1s infinite">Listening...</span>';
  } catch (error) {
    log(`Error starting listener: ${error}`);
  }
});

stopListenBtn.addEventListener("click", async () => {
  try {
    await invoke("stop_listening");
    startListenBtn.disabled = false;
    stopListenBtn.disabled = true;
    inputDeviceSelect.disabled = false;
    decodedDisplay.innerHTML = '<span style="color: #555">Stopped</span>';
    log("Listener stopped.");
  } catch (error) {
    log(`Error stopping listener: ${error}`);
  }
});

startTestBtn.addEventListener("click", async () => {
  const index = parseInt(outputDeviceSelect.value);
  if (isNaN(index)) {
    log("Please select an output device.");
    return;
  }

  try {
    await invoke("start_test_sequence", { deviceIndex: index });
    startTestBtn.disabled = true;
    stopTestBtn.disabled = false;
    outputDeviceSelect.disabled = true;
  } catch (error) {
    log(`Error starting test: ${error}`);
  }
});

stopTestBtn.addEventListener("click", async () => {
  try {
    await invoke("stop_playing");
    startTestBtn.disabled = false;
    stopTestBtn.disabled = true;
    outputDeviceSelect.disabled = false;
    log("Test sequence stopped.");
  } catch (error) {
    log(`Error stopping test: ${error}`);
  }
});

// Backend Events
interface LogEvent {
  message: string;
}

interface DecodedFrameEvent {
  frame_type: string;
  payload: string;
}

await listen<LogEvent>("log", (event) => {
  log(event.payload.message);
});

await listen<DecodedFrameEvent>("decoded_frame", (event) => {
  const { frame_type, payload } = event.payload;
  decodedDisplay.textContent = `${frame_type}: ${payload}`;
  // Flash effect
  decodedDisplay.style.backgroundColor = "#222";
  setTimeout(() => (decodedDisplay.style.backgroundColor = "#000"), 100);
});

await listen<number>("input_level", (event) => {
  const rms = event.payload;
  // Visual gain factor to make signals visible
  let percent = Math.min(rms * 400, 100);
  inputMeter.style.width = `${percent}%`;

  if (percent > 90) {
    inputMeter.style.backgroundColor = "#ff4444";
  } else {
    inputMeter.style.backgroundColor = "#00ff00";
  }
});

// Init
window.addEventListener("DOMContentLoaded", () => {
  loadDevices();
});

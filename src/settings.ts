import { invoke } from "@tauri-apps/api/core";

const keyInput = document.getElementById("key") as HTMLInputElement;
const status = document.getElementById("status")!;
const saveBtn = document.getElementById("save") as HTMLButtonElement;
const clearBtn = document.getElementById("clear") as HTMLButtonElement;

async function refreshStatus() {
  const has = await invoke<boolean>("has_admin_key");
  status.textContent = has
    ? "An admin key is saved in Windows Credential Manager."
    : "No admin key saved — the API spend row is hidden.";
}

async function withButtonsLocked(action: () => Promise<void>, failMsg: string) {
  saveBtn.disabled = clearBtn.disabled = true;
  try {
    await action();
  } catch (e) {
    status.textContent = failMsg + e;
  } finally {
    saveBtn.disabled = clearBtn.disabled = false;
  }
}

saveBtn.addEventListener("click", () =>
  withButtonsLocked(async () => {
    const key = keyInput.value.trim();
    await invoke("set_admin_key", { key });
    keyInput.value = "";
    await refreshStatus();
    if (key) {
      status.textContent += " Saved.";
    }
  }, "Failed to save: "),
);

clearBtn.addEventListener("click", () =>
  withButtonsLocked(async () => {
    await invoke("set_admin_key", { key: "" });
    await refreshStatus();
  }, "Failed to clear: "),
);

refreshStatus();

const mkeyInput = document.getElementById("mkey") as HTMLInputElement;
const mstatus = document.getElementById("mstatus")!;
const msaveBtn = document.getElementById("msave") as HTMLButtonElement;
const mclearBtn = document.getElementById("mclear") as HTMLButtonElement;

async function refreshMoonshotStatus() {
  try {
    const has = await invoke<boolean>("has_moonshot_key");
    mstatus.textContent = has
      ? "A Moonshot key is saved in your OS secret store."
      : "No Moonshot key saved — using Kimi Code's key if it has one.";
  } catch (e) {
    mstatus.textContent = "Failed to read Moonshot key status: " + e;
  }
}

async function withMoonshotLocked(action: () => Promise<void>, failMsg: string) {
  msaveBtn.disabled = mclearBtn.disabled = true;
  try {
    await action();
  } catch (e) {
    mstatus.textContent = failMsg + e;
  } finally {
    msaveBtn.disabled = mclearBtn.disabled = false;
  }
}

msaveBtn.addEventListener("click", () =>
  withMoonshotLocked(async () => {
    const key = mkeyInput.value.trim();
    await invoke("set_moonshot_key", { key });
    mkeyInput.value = "";
    await refreshMoonshotStatus();
    if (key) {
      mstatus.textContent += " Saved.";
    }
  }, "Failed to save: "),
);

mclearBtn.addEventListener("click", () =>
  withMoonshotLocked(async () => {
    await invoke("set_moonshot_key", { key: "" });
    await refreshMoonshotStatus();
  }, "Failed to clear: "),
);

refreshMoonshotStatus();

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

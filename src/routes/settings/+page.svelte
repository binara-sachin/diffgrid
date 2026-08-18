<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { IntraLineMode, Settings } from "$lib/types";

  // M4's settings window (docs/UI/ui-02.png). Scoped to just the "Diff & merge > Comparison"
  // group, the only settings this app actually implements -- see DECISIONS.md for why the
  // mockup's other nav categories (General/Editor/Filters/Version control/Shortcuts) and the
  // Merge section aren't built here: there's no content to put in them yet, and empty
  // placeholder scaffolding isn't worth the code.
  let settings: Settings = $state({ ignoreWhitespace: false, ignoreCase: false, collapseContextLines: 3, intraLineMode: "character" });
  let loaded = $state(false);

  onMount(async () => {
    window.addEventListener("error", (e) => {
      invoke("report_error", { message: `settings window.onerror: ${e.message}` });
    });
    window.addEventListener("unhandledrejection", (e) => {
      invoke("report_error", { message: `settings unhandledrejection: ${String(e.reason)}` });
    });
    settings = await invoke<Settings>("load_settings");
    loaded = true;
  });

  // Persists on every change (no explicit Save button, matching the mockup's live-toggle
  // switches) and notifies the main window via the settings-changed event Rust emits after a
  // successful save -- see src-tauri/src/lib.rs's save_settings.
  async function persist() {
    if (!loaded) return; // don't overwrite the real file with placeholder defaults before load_settings has resolved
    await invoke("save_settings", { settings });
  }

  const intraLineModes: { value: IntraLineMode; label: string }[] = [
    { value: "off", label: "Off" },
    { value: "word", label: "Word" },
    { value: "character", label: "Character" },
  ];

  const contextLineOptions = [0, 1, 3, 5, 10, 20];
</script>

<main>
  <h1>Comparison</h1>

  <div class="row">
    <div class="row-text">
      <div class="row-title">Ignore whitespace changes</div>
      <div class="row-desc">Lines differing only in spacing are treated as identical</div>
    </div>
    <input type="checkbox" bind:checked={settings.ignoreWhitespace} onchange={persist} />
  </div>

  <div class="row">
    <div class="row-text">
      <div class="row-title">Ignore case</div>
    </div>
    <input type="checkbox" bind:checked={settings.ignoreCase} onchange={persist} />
  </div>

  <div class="row">
    <div class="row-text">
      <div class="row-title">Collapse unchanged regions</div>
      <div class="row-desc">Keep this many context lines around every change</div>
    </div>
    <select bind:value={settings.collapseContextLines} onchange={persist} class="context-select">
      {#each contextLineOptions as n (n)}
        <option value={n}>{n} {n === 1 ? "line" : "lines"}</option>
      {/each}
    </select>
  </div>

  <div class="row">
    <div class="row-text">
      <div class="row-title">Highlight changes within a line</div>
    </div>
    <div class="segmented">
      {#each intraLineModes as m (m.value)}
        <button class:active={settings.intraLineMode === m.value} onclick={() => { settings.intraLineMode = m.value; persist(); }}>
          {m.label}
        </button>
      {/each}
    </div>
  </div>
</main>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    height: 100%;
  }
  main {
    font-family: -apple-system, ui-sans-serif, system-ui, sans-serif;
    padding: 24px 28px;
    color: #1a1a1a;
  }
  h1 {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: #888;
    margin: 0 0 12px 0;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 14px 0;
    border-bottom: 1px solid #eee;
  }
  .row-title {
    font-size: 14px;
  }
  .row-desc {
    font-size: 12px;
    color: #888;
    margin-top: 2px;
  }
  .context-select {
    padding: 4px 6px;
    font-size: 13px;
  }
  .segmented {
    display: flex;
    border: 1px solid #ccc;
    border-radius: 6px;
    overflow: hidden;
  }
  .segmented button {
    background: #f5f5f5;
    border: none;
    padding: 6px 14px;
    font-size: 13px;
    cursor: pointer;
    border-right: 1px solid #ccc;
  }
  .segmented button:last-child {
    border-right: none;
  }
  .segmented button.active {
    background: #fff;
    font-weight: 600;
  }

  @media (prefers-color-scheme: dark) {
    main {
      color: #eee;
      background: #1e1e1e;
    }
    h1 {
      color: #999;
    }
    .row {
      border-bottom-color: #333;
    }
    .row-desc {
      color: #999;
    }
    .segmented {
      border-color: #444;
    }
    .segmented button {
      background: #2a2a2a;
      color: #eee;
      border-right-color: #444;
    }
    .segmented button.active {
      background: #3a3a3a;
    }
  }
</style>

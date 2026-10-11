(function () {
  var NL = String.fromCharCode(10);
  var core = (window.__TAURI__ && window.__TAURI__.core) || null;
  var evt = (window.__TAURI__ && window.__TAURI__.event) || null;
  var logEl = document.getElementById("log");

  function log(line) {
    logEl.textContent = line + NL + NL + logEl.textContent;
  }

  function invoke(cmd, args) {
    if (!core) {
      log("[error] window.__TAURI__ 未注入（请通过 c96-gui 启动）");
      return Promise.reject(new Error("no tauri"));
    }
    return core.invoke(cmd, args);
  }

  function click(id, cmd, args) {
    document.getElementById(id).addEventListener("click", function () {
      invoke(cmd, args || {})
        .then(function (msg) { log("[" + cmd + "]" + NL + JSON.stringify(msg, null, 2)); })
        .catch(function (e) { log("[" + cmd + "] error: " + e); });
    });
  }

  document.getElementById("open").addEventListener("click", function () {
    var url = document.getElementById("url").value.trim();
    invoke("open_browser", { url: url })
      .then(function (msg) { log("[open_browser] " + msg); })
      .catch(function (e) { log("[open_browser] error: " + e); });
  });

  // E2：实时显示自控窗口回传的点击/跳转/加载事件。
  if (evt) {
    evt.listen("browser://event", function (e) {
      var p = e.payload || {};
      var line = "[event] #" + p.seq + "  " + p.kind + "  " + (p.url || "");
      if (p.detail) { line = line + NL + "         " + p.detail; }
      log(line);
    });
  } else {
    log("[warn] window.__TAURI__.event 不可用，事件实时列表禁用");
  }

  click("snapshot", "snapshot_probe");
  click("banner", "banner");
  click("rawinfo", "webview2_info", { label: "browser" });
  click("rawzoom", "webview2_zoom", { label: "browser", factor: 1.5 });
  click("rawmain", "webview2_info", { label: "main" });
  click("events", "browser_events");
  click("clear", "browser_events_clear");
})();

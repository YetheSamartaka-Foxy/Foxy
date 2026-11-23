(function () {
  var REPO = "YetheSamartaka-Foxy/Foxy";
  var RELEASES_URL = "https://github.com/" + REPO + "/releases/latest";
  var API_URL = "https://api.github.com/repos/" + REPO + "/releases/latest";

  var PLATFORMS = {
    windows: {
      label: "Windows installer",
      exact: function (v) { return "Foxy-" + v + "-setup.exe"; },
      fallback: function (name) {
        return /^Foxy-.*-setup\.exe$/i.test(name) && !/cli/i.test(name);
      }
    },
    linux: {
      label: "Linux x86_64 installer",
      exact: function (v) { return "Foxy-" + v + "-linux-x86_64-installer.sh"; },
      fallback: function (name) {
        return /^Foxy-.*-linux-x86_64-installer\.sh$/i.test(name);
      }
    },
    "linux-arm64": {
      label: "Linux ARM64 installer",
      exact: function (v) { return "Foxy-" + v + "-linux-aarch64-installer.sh"; },
      fallback: function (name) {
        return /^Foxy-.*-linux-aarch64-installer\.sh$/i.test(name);
      }
    },
    macos: {
      label: "macOS Apple Silicon disk image",
      exact: function (v) { return "Foxy-" + v + "-macos-arm64.dmg"; },
      fallback: function (name) {
        return /^Foxy-.*-macos-arm64\.dmg$/i.test(name);
      }
    }
  };

  function detectPlatform() {
    var ua = (navigator.userAgent || "").toLowerCase();
    var plat = (navigator.platform || "").toLowerCase();
    if (ua.indexOf("mac") !== -1 || plat.indexOf("mac") !== -1) {
      return "macos";
    }
    if (ua.indexOf("linux") !== -1 || plat.indexOf("linux") !== -1) {
      if (ua.indexOf("aarch64") !== -1 || ua.indexOf("arm64") !== -1 || plat.indexOf("aarch64") !== -1) {
        return "linux-arm64";
      }
      return "linux";
    }
    return "windows";
  }

  function requestedPlatform() {
    var attr = document.documentElement.getAttribute("data-platform");
    if (attr && PLATFORMS[attr]) {
      return attr;
    }
    var params = new URLSearchParams(window.location.search);
    var query = (params.get("platform") || "").toLowerCase();
    if (query && PLATFORMS[query]) {
      return query;
    }
    return detectPlatform();
  }

  function pickAsset(assets, version, spec) {
    var exactName = spec.exact(version);
    for (var i = 0; i < assets.length; i++) {
      if (assets[i].name === exactName) {
        return assets[i];
      }
    }
    for (var j = 0; j < assets.length; j++) {
      if (spec.fallback(assets[j].name || "")) {
        return assets[j];
      }
    }
    return null;
  }

  var statusEl = document.getElementById("status");
  var linkEl = document.getElementById("direct-link");
  var platform = requestedPlatform();
  var spec = PLATFORMS[platform];

  function setStatus(text) {
    if (statusEl) {
      statusEl.textContent = text;
    }
  }

  setStatus("Looking up the latest Foxy " + spec.label + "...");

  fetch(API_URL, { headers: { Accept: "application/vnd.github+json" } })
    .then(function (response) {
      if (!response.ok) {
        throw new Error("HTTP " + response.status);
      }
      return response.json();
    })
    .then(function (release) {
      var version = (release.tag_name || "").replace(/^v/, "");
      var assets = Array.isArray(release.assets) ? release.assets : [];
      var asset = pickAsset(assets, version, spec);
      if (!asset || !asset.browser_download_url) {
        setStatus("No " + spec.label + " was found on the latest release. Use the release page link below.");
        return;
      }
      if (linkEl) {
        linkEl.href = asset.browser_download_url;
        linkEl.textContent = "Download " + asset.name;
      }
      setStatus("Starting download of " + asset.name + " (Foxy " + version + ")...");
      window.location.replace(asset.browser_download_url);
    })
    .catch(function () {
      if (linkEl) {
        linkEl.href = RELEASES_URL;
        linkEl.textContent = "Open the latest Foxy release on GitHub";
      }
      setStatus("Could not resolve the direct download automatically. Open the latest release page below.");
    });
})();

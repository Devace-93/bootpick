// The "Download" button: detects the visitor's operating system and, when a
// ready-made package exists for it, downloads that package on click.
// Otherwise the button leads to the install section.
//
// The packages come from the project's GitHub releases, so a new release (or
// the first one with a Windows package) is picked up without touching the
// site. Until that list arrives, and if it cannot be read, the packages named
// in FALLBACK are used.
(function () {
  const REPO = 'Devace-93/bootpick';
  // File name endings of the packages made by tools/package.sh and tools/package.ps1.
  const SUFFIX = { linux: '-x86_64-linux.tar.gz', windows: '-x86_64-windows.zip' };
  const NAMES = { linux: 'Linux', windows: 'Windows' };
  const FALLBACK = {
    linux: 'https://github.com/' + REPO + '/releases/download/v0.2.0-beta/bootpick-v0.2.0-beta-x86_64-linux.tar.gz',
  };

  // Only systems bootpick can be installed from; phones, tablets and Macs get
  // the plain button.
  function detectOS() {
    const ua = navigator.userAgent || '';
    const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '';
    if (/android|iphone|ipad|cros/i.test(ua)) return null;
    if (/win/i.test(platform) || /windows/i.test(ua)) return 'windows';
    if (/linux/i.test(platform) || /linux|x11/i.test(ua)) return 'linux';
    return null;
  }

  const os = detectOS();
  const urls = Object.assign({}, FALLBACK);

  function render() {
    const t = window.BootpickI18n.t;
    const url = os ? urls[os] : null;
    document.querySelectorAll('[data-download]').forEach((a) => {
      if (url) {
        a.href = url;
        a.textContent = t('ctaDownloadFor').replace('{os}', NAMES[os]);
      } else {
        a.href = '#install';
        a.textContent = t('ctaDownload');
      }
    });
  }

  // The newest release (pre-releases included) that carries each package.
  function loadReleases() {
    if (!window.fetch) return;
    fetch('https://api.github.com/repos/' + REPO + '/releases?per_page=20', { headers: { Accept: 'application/vnd.github+json' } })
      .then((r) => (r.ok ? r.json() : []))
      .then((releases) => {
        const found = {};
        (Array.isArray(releases) ? releases : []).forEach((rel) => {
          if (rel.draft) return;
          Object.keys(SUFFIX).forEach((k) => {
            if (found[k]) return;
            const asset = (rel.assets || []).find((x) => typeof x.name === 'string' && x.name.endsWith(SUFFIX[k]));
            if (asset) found[k] = asset.browser_download_url;
          });
        });
        Object.assign(urls, found);
        render();
      })
      .catch(() => {});
  }

  window.BootpickDownload = { render };
  // i18n.js announces every language change (and its first run).
  document.addEventListener('bootpick:lang', render);
  loadReleases();
})();

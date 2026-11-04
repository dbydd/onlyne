'use strict';

const path = require('node:path');
const { app, BrowserWindow, Menu, dialog, ipcMain, shell } = require('electron');

const APP_NAME = 'Onlyne Desktop';
const REPO_URL = 'https://github.com/dbydd/onlyne';
const LOOPBACK_HOSTS = {
  '127.0.0.1': true,
  localhost: true,
  '[::1]': true,
  '::1': true,
};

/** Origin of the loaded app; set once from ONLYNE_DESKTOP_URL. */
let appOrigin = null;
/** One-use /launch/<secret> URL, consumed by the first window. */
let pendingLaunchUrl = null;

function loopbackUrl(raw) {
  if (typeof raw !== 'string' || raw === '') return null;
  let url;
  try { url = new URL(raw); } catch { return null; }
  if (url.protocol !== 'http:') return null;
  if (LOOPBACK_HOSTS[url.hostname] !== true) return null;
  if (url.port === '' || url.username !== '' || url.password !== '') return null;
  return url;
}

function externalUrl(raw) {
  if (typeof raw !== 'string' || raw === '') return null;
  let url;
  try { url = new URL(raw); } catch { return null; }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
  if (url.hostname === '') return null;
  return url;
}

function originOf(raw) {
  try { return new URL(raw).origin; } catch { return 'the backend'; }
}

function isAppOrigin(raw) {
  if (appOrigin === null) return false;
  return originOf(raw) === appOrigin.origin;
}

function openExternal(url) {
  shell.openExternal(url.href).catch(error => {
    console.error(`Onlyne Desktop: could not open ${url.href}: ${error instanceof Error ? error.message : String(error)}`);
  });
}

function openExternalFromApp(raw) {
  const target = externalUrl(raw);
  if (target === null) return;
  if (target.origin === appOrigin.origin) return;
  openExternal(target);
}

function guardContents(contents) {
  contents.setWindowOpenHandler(({ url }) => {
    openExternalFromApp(url);
    return { action: 'deny' };
  });
  contents.on('will-navigate', (event, url) => {
    if (isAppOrigin(url)) return;
    event.preventDefault();
    openExternalFromApp(url);
  });
  contents.on('will-redirect', (event, url) => {
    if (isAppOrigin(url)) return;
    event.preventDefault();
    openExternalFromApp(url);
  });
  contents.on('will-attach-webview', event => event.preventDefault());
}

function createWindow() {
  if (appOrigin === null) return undefined;
  const url = pendingLaunchUrl === null ? appOrigin.href : pendingLaunchUrl;
  pendingLaunchUrl = null;
  const window = new BrowserWindow({
    title: APP_NAME,
    width: 1280,
    height: 860,
    minWidth: 900,
    minHeight: 640,
    show: false,
    backgroundColor: '#14161a',
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webviewTag: false,
    },
  });
  window.once('ready-to-show', () => window.show());
  window.webContents.on('did-fail-load', (_event, code, description, failedUrl, isMainFrame) => {
    if (isMainFrame !== true) return;
    console.error(`Onlyne Desktop: window failed to load from ${originOf(failedUrl)} (${code} ${description})`);
    window.show();
  });
  guardContents(window.webContents);
  window.loadURL(url).catch(() => {
    console.error('Onlyne Desktop: the window could not load the backend URL.');
  });
  return window;
}

function focusExistingWindow() {
  const window = BrowserWindow.getAllWindows()[0];
  if (window === undefined) {
    createWindow();
    return;
  }
  if (window.isMinimized()) window.restore();
  window.show();
  window.focus();
}

function buildMenu() {
  const isMac = process.platform === 'darwin';
  return Menu.buildFromTemplate([
    ...(isMac
      ? [{
        label: APP_NAME,
        submenu: [
          { role: 'about' },
          { type: 'separator' },
          { role: 'hide' },
          { role: 'hideOthers' },
          { role: 'unhide' },
          { type: 'separator' },
          { role: 'quit' },
        ],
      }]
      : []),
    {
      label: 'File',
      submenu: [
        ...(isMac ? [{ role: 'close' }] : [{ role: 'quit' }]),
      ],
    },
    {
      label: 'Edit',
      submenu: [
        { role: 'undo' },
        { role: 'redo' },
        { type: 'separator' },
        { role: 'cut' },
        { role: 'copy' },
        { role: 'paste' },
        ...(isMac ? [{ role: 'pasteAndMatchStyle' }] : []),
        { role: 'delete' },
        { type: 'separator' },
        { role: 'selectAll' },
      ],
    },
    {
      label: 'View',
      submenu: [
        { role: 'reload' },
        { role: 'toggleDevTools' },
        { type: 'separator' },
        { role: 'resetZoom' },
        { role: 'zoomIn' },
        { role: 'zoomOut' },
        { type: 'separator' },
        { role: 'togglefullscreen' },
      ],
    },
    {
      label: 'Window',
      submenu: [
        { role: 'minimize' },
        { role: 'zoom' },
        ...(isMac ? [{ type: 'separator' }, { role: 'front' }] : [{ role: 'close' }]),
      ],
    },
    {
      role: 'help',
      submenu: [
        { label: 'Onlyne on GitHub', click: () => openExternal(new URL(REPO_URL)) },
        { label: 'Report an Issue', click: () => openExternal(new URL(`${REPO_URL}/issues`)) },
      ],
    },
  ]);
}

function start() {
  const raw = process.env.ONLYNE_DESKTOP_URL;
  const launchUrl = raw === undefined ? null : loopbackUrl(raw);
  if (launchUrl === null) {
    dialog.showErrorBox(
      APP_NAME,
      'This window must be started by the Onlyne launcher: ONLYNE_DESKTOP_URL is missing or is not a loopback HTTP URL.',
    );
    app.exit(1);
    return;
  }
  appOrigin = new URL('/', launchUrl);
  pendingLaunchUrl = launchUrl.href;
  ipcMain.handle('onlyne:open-external', (event, url) => {
    if (!isAppOrigin(event.senderFrame?.url)) return false;
    const target = externalUrl(url);
    if (target === null || target.origin === appOrigin.origin) return false;
    openExternal(target);
    return true;
  });
  Menu.setApplicationMenu(buildMenu());
  const window = createWindow();
  if (window !== undefined) {
    // The launcher publishes its lock only after this line: a second launch must find a
    // window that already owns the single-instance lock.
    process.stdout.write('{"type":"desktop.shell.ready"}\n');
  }
}

app.setName(APP_NAME);

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', focusExistingWindow);
  app.whenReady().then(start);
  app.on('activate', () => {
    if (app.isReady() && BrowserWindow.getAllWindows().length === 0) createWindow();
  });
  app.on('window-all-closed', () => {
    if (process.platform !== 'darwin') app.quit();
  });
}

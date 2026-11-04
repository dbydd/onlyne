'use strict';

const { contextBridge, ipcRenderer } = require('electron');

// window.open and <a target> clicks are already mediated by the main process; this
// bridge lets renderer plugins hand an external link to the OS explicitly.
contextBridge.exposeInMainWorld('onlyneShell', Object.freeze({
  openExternal: url => ipcRenderer.invoke('onlyne:open-external', String(url)).then(Boolean, () => false),
}));

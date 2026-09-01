export const IPC_CHANNELS = {
  copyText: "gateway:copy-text",
  getState: "gateway:get-state",
  listReceipts: "gateway:list-receipts",
  start: "gateway:start",
  stateChanged: "gateway:state-changed",
  stop: "gateway:stop",
} as const;

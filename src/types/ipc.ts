export type ThemeId = "image" | "music" | "video" | "novel" | "document";

export type StallChoice = ThemeId | "site" | "app";

export type ScanReport = {
  root: string;
  folderName: string;
  hasIndex: boolean;
  truncated: boolean;
  counts: Record<ThemeId | "other", number>;
};

export type HostInfo = {
  shareCode: string;
  endpointId: string;
  localUrl: string;
  root: string;
  relayReady: boolean;
  modeLabel: string;
};

export type StageInfo = {
  label: string;
  title: string;
  kind: "preview" | "guest";
  endpointId?: string | null;
};

export type GuestInfo = StageInfo;

export type PlazaStall = {
  endpointId: string;
  title: string;
  label: string;
  blurb: string;
};

export type PlazaSnapshot = {
  stalls: PlazaStall[];
  neighbors: number;
};

export type TrafficSnapshot = {
  sent: number;
  received: number;
};

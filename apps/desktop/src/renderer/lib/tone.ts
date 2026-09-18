export type Tone = "success" | "warning" | "danger" | "neutral";

export const toneTextClass: Record<Tone, string> = {
  success: "text-primary",
  warning: "text-warning",
  danger: "text-destructive",
  neutral: "text-muted-foreground",
};

export const toneDotClass: Record<Tone, string> = {
  success: "bg-primary",
  warning: "bg-warning",
  danger: "bg-destructive",
  neutral: "bg-muted-foreground",
};

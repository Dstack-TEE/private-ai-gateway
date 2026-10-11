import phalaServiceIcon from "../assets/service-phala.svg";
import redpillServiceIcon from "../assets/service-redpill.png";
import type { ServiceProvider } from "../../shared/contracts";

/** Custom services have no logo. */
export const SERVICE_ICONS: Record<ServiceProvider, string | undefined> = {
  phala: phalaServiceIcon,
  redpill: redpillServiceIcon,
  custom: undefined,
};

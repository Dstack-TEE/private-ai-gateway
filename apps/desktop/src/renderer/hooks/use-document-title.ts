import { useEffect } from "react";
import { brand } from "../brand/brand";

/** Names the browser tab after the page, such as "Usage · Private AI Proxy". */
export function useDocumentTitle(page: string | undefined): void {
  useEffect(() => {
    document.title = page ? `${page} · ${brand.productName}` : brand.productName;
  }, [page]);
}

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AppLogo } from "../components/BrandMark";

describe("brand mark", () => {
  it("renders the full logo with an accessible name", () => {
    const { getByRole } = render(<AppLogo />);
    expect(getByRole("img", { name: "Agendar" })).toBeTruthy();
  });
});

import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import SizeRangeInputs from "./SizeRangeInputs";
import { useFiltersStore, DEFAULT_FILTERS } from "@/stores/filters";

vi.mock("@/i18n/hooks", () => ({
  useTranslation: () => ({
    t: (key: string) => key,
    i18n: { language: "en" },
  }),
}));

describe("SizeRangeInputs", () => {
  beforeEach(() => {
    useFiltersStore.setState({
      filters: { ...DEFAULT_FILTERS },
      showAdvanced: false,
    });
  });

  it("renders both bounds", () => {
    render(<SizeRangeInputs />);
    expect(screen.getByLabelText("filters.size_min_aria")).toBeInTheDocument();
    expect(screen.getByLabelText("filters.size_max_aria")).toBeInTheDocument();
  });

  it("starts empty so the unfiltered path stays untouched", () => {
    render(<SizeRangeInputs />);
    expect(screen.getByLabelText("filters.size_min_aria")).toHaveValue("");
    expect(screen.getByLabelText("filters.size_max_aria")).toHaveValue("");
  });

  it("does not commit while typing", () => {
    // Regression: committing per keystroke made the backend rescan for "1",
    // "10", "100" and "1000" in turn. Typing alone must change nothing.
    render(<SizeRangeInputs />);
    const input = screen.getByLabelText("filters.size_min_aria");
    fireEvent.change(input, { target: { value: "1" } });
    fireEvent.change(input, { target: { value: "10" } });
    fireEvent.change(input, { target: { value: "1000" } });
    expect(useFiltersStore.getState().filters.size_min_mb).toBe("");
  });

  it("commits on Enter", () => {
    render(<SizeRangeInputs />);
    const input = screen.getByLabelText("filters.size_min_aria");
    fireEvent.change(input, { target: { value: "1000" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(useFiltersStore.getState().filters.size_min_mb).toBe("1000");
  });

  it("commits on blur", () => {
    render(<SizeRangeInputs />);
    const input = screen.getByLabelText("filters.size_max_aria");
    fireEvent.change(input, { target: { value: "500" } });
    fireEvent.blur(input);
    expect(useFiltersStore.getState().filters.size_max_mb).toBe("500");
  });

  it("resets pagination when a bound is committed", () => {
    useFiltersStore.setState({
      filters: { ...DEFAULT_FILTERS, page: 7 },
      showAdvanced: false,
    });
    render(<SizeRangeInputs />);
    const input = screen.getByLabelText("filters.size_min_aria");
    fireEvent.change(input, { target: { value: "1000" } });
    fireEvent.keyDown(input, { key: "Enter" });
    // Page 7 of the old filter means nothing under a new one.
    expect(useFiltersStore.getState().filters.page).toBe(1);
  });

  it("does not reset pagination when the value is unchanged", () => {
    useFiltersStore.setState({
      filters: { ...DEFAULT_FILTERS, size_min_mb: "1000", page: 5 },
      showAdvanced: false,
    });
    render(<SizeRangeInputs />);
    const input = screen.getByLabelText("filters.size_min_aria");
    fireEvent.blur(input);
    expect(useFiltersStore.getState().filters.page).toBe(5);
  });

  it("Escape abandons the edit", () => {
    render(<SizeRangeInputs />);
    const input = screen.getByLabelText("filters.size_min_aria");
    fireEvent.change(input, { target: { value: "9999" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(useFiltersStore.getState().filters.size_min_mb).toBe("");
  });
});
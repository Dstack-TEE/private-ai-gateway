import { createContext, useContext } from "react";
import { setMonth, setYear, subMonths } from "date-fns";
import { useDayPicker, type DropdownProps, type MonthCaptionProps } from "react-day-picker";
import { ChoiceSelect } from "./choice-select";

const CaptionContext = createContext<Pick<MonthCaptionProps, "calendarMonth" | "displayIndex"> | null>(null);

function MonthCaption({ calendarMonth, displayIndex, ...props }: MonthCaptionProps) {
  return <CaptionContext.Provider value={{ calendarMonth, displayIndex }}><div {...props} /></CaptionContext.Provider>;
}

function CalendarSelect({ options = [], value, disabled, "aria-label": label, kind }: DropdownProps & { kind: "month" | "year" }) {
  const caption = useContext(CaptionContext);
  const { goToMonth, months, dayPickerProps } = useDayPicker();
  return <ChoiceSelect size="sm" className="relative" label={label ?? (kind === "month" ? "Month" : "Year")}
    value={String(value ?? "")} disabled={disabled}
    options={options.map((option) => ({ ...option, value: String(option.value) }))}
    onChange={(next) => {
      if (!caption) return;
      const date = (kind === "month" ? setMonth : setYear)(caption.calendarMonth.date, Number(next));
      const offset = dayPickerProps.reverseMonths ? months.length - 1 - caption.displayIndex : caption.displayIndex;
      goToMonth(subMonths(date, offset));
    }} />;
}

export const calendarSelectComponents = {
  MonthCaption,
  MonthsDropdown: (props: DropdownProps) => <CalendarSelect {...props} kind="month" />,
  YearsDropdown: (props: DropdownProps) => <CalendarSelect {...props} kind="year" />,
};

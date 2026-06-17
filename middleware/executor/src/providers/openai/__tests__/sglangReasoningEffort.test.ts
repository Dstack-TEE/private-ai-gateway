import { mapSglangReasoningEffort } from '../chatComplete';
import { Params } from '../../../types/requestBody';

const effort = (value?: string): Params =>
  ({ messages: [], ...(value !== undefined ? { reasoning_effort: value } : {}) }) as Params;

describe('mapSglangReasoningEffort', () => {
  it('normalizes OpenAI-only values onto the sglang vocabulary', () => {
    expect(mapSglangReasoningEffort(effort('minimal'))).toBe('low');
    expect(mapSglangReasoningEffort(effort('xhigh'))).toBe('max');
  });

  it('passes sglang-accepted values through unchanged', () => {
    for (const value of ['none', 'low', 'medium', 'high', 'max']) {
      expect(mapSglangReasoningEffort(effort(value))).toBe(value);
    }
  });

  it('leaves a missing reasoning_effort untouched', () => {
    expect(mapSglangReasoningEffort(effort())).toBeUndefined();
  });
});

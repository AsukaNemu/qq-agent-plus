import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  imageUnderstandingEnabled,
  modelImageVerdict,
  visionModelOverride
} from '../src/llm/vision-scan.js';

test('hy4-preview-f is treated as a text-only model', () => {
  assert.equal(modelImageVerdict('workbuddy_local', 'hy4-preview-f'), 'no-vision');
  assert.equal(modelImageVerdict('', 'hy4-preview-f'), 'no-vision');
});

test('a dedicated vision model can backstop a text-only main model', () => {
  const cfg = {
    api: {
      vision: true,
      provider: 'workbuddy_local',
      model: 'hy4-preview-f',
      visionModel: {
        enabled: true,
        provider: 'ark',
        model: 'ep-20260929014118-cnqdv',
        timeoutMs: 90000
      }
    },
    providers: [{ id: 'ark', baseURL: 'https://example.test/v1' }],
    providerKeys: { ark: 'secret' }
  };
  const override = visionModelOverride(cfg);
  assert.deepEqual(
    { baseUrl: override.baseUrl, model: override.model, timeoutMs: override.timeoutMs },
    { baseUrl: 'https://example.test/v1', model: 'ep-20260929014118-cnqdv', timeoutMs: 90000 }
  );
  assert.equal(override.apiKey, 'secret');
  assert.equal(imageUnderstandingEnabled(cfg), true);
  assert.equal(imageUnderstandingEnabled({ ...cfg, api: { ...cfg.api, vision: false } }), false);
});

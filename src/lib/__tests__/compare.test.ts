import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fetchComfortScores } from '../comfort-index';
import { ComfortResult } from '../../types';

// Mock the modules
vi.mock('../modules/air', () => ({
  airModule: {
    id: 'air',
    enabled: true,
    fetch: vi.fn().mockResolvedValue(50),
    normalize: vi.fn().mockReturnValue(80),
  }
}));

vi.mock('../modules/weather', () => ({
  weatherModule: {
    id: 'temperature',
    enabled: true,
    fetch: vi.fn().mockResolvedValue(25),
    normalize: vi.fn().mockReturnValue(90),
  }
}));

vi.mock('../modules/uv', () => ({
  uvModule: {
    id: 'uv',
    enabled: true,
    fetch: vi.fn().mockResolvedValue(5),
    normalize: vi.fn().mockReturnValue(70),
  }
}));

vi.mock('../modules/earthquake', () => ({
  earthquakeModule: {
    id: 'earthquake',
    enabled: false,
    fetch: vi.fn(),
    normalize: vi.fn(),
  }
}));

vi.mock('../modules/marine', () => ({
  marineModule: {
    id: 'sea',
    enabled: true,
    fetch: vi.fn().mockResolvedValue(22),
    normalize: vi.fn().mockReturnValue(85),
  }
}));

describe('fetchComfortScores', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('should fetch and normalize scores for enabled modules', async () => {
    const result = await fetchComfortScores(36.27, 32.32);

    expect(result).toBeDefined();
    expect(result.air).toBeDefined();
    expect(result.temperature).toBeDefined();
    expect(result.uv).toBeDefined();
    expect(result.sea).toBeDefined();
    
    // Earthquake is disabled
    expect(result.earthquake).toBeUndefined();

    // Check structure of a subscore
    expect(result.air).toEqual({
      id: 'air',
      value: 50,
      normalized: 80,
      weight: expect.any(Number),
    });
  });

  it('should handle CompareResult type correctly', () => {
    // This is just a type check test to ensure the type is exported and usable
    const mockResult: ComfortResult = {
      totalScore: 85,
      subScores: {
        air: { id: 'air', value: 50, normalized: 80, weight: 0.22 }
      }
    };

    const compareResult: import('../../types').CompareResult = {
      cities: {
        gazipasha: mockResult
      }
    };

    expect(compareResult.cities.gazipasha.totalScore).toBe(85);
  });
});

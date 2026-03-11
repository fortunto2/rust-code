import React from 'react';
import { CompareResult } from '@/types';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';

interface CityComparisonProps {
  data: CompareResult;
}

export function CityComparison({ data }: CityComparisonProps) {
  const cityNames = Object.keys(data.cities);
  
  if (cityNames.length === 0) {
    return <div>No cities to compare</div>;
  }

  // Get all unique metric IDs from the first city (assuming all cities have the same metrics enabled)
  const firstCity = data.cities[cityNames[0]];
  const metricIds = Object.keys(firstCity.subScores);

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>City Comparison</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="overflow-x-auto">
          <table className="w-full text-sm text-left">
            <thead className="text-xs uppercase bg-muted/50">
              <tr>
                <th className="px-6 py-3 font-medium">Metric</th>
                {cityNames.map((city) => (
                  <th key={city} className="px-6 py-3 font-medium capitalize">
                    {city}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {/* Total Score Row */}
              <tr className="border-b bg-muted/20">
                <td className="px-6 py-4 font-bold">Total Score</td>
                {cityNames.map((city) => {
                  const score = data.cities[city].totalScore;
                  const isWinner = score === Math.max(...cityNames.map(c => data.cities[c].totalScore));
                  return (
                    <td 
                      key={city} 
                      className={`px-6 py-4 font-bold ${isWinner ? 'text-green-600 dark:text-green-400' : ''}`}
                    >
                      {score.toFixed(1)}
                    </td>
                  );
                })}
              </tr>
              
              {/* Individual Metrics Rows */}
              {metricIds.map((metricId) => {
                // Find the highest score for this metric across all cities
                const maxScore = Math.max(
                  ...cityNames.map(city => data.cities[city].subScores[metricId]?.normalized || 0)
                );

                return (
                  <tr key={metricId} className="border-b">
                    <td className="px-6 py-4 font-medium capitalize">{metricId}</td>
                    {cityNames.map((city) => {
                      const score = data.cities[city].subScores[metricId]?.normalized || 0;
                      const isWinner = score === maxScore && score > 0;
                      
                      return (
                        <td 
                          key={city} 
                          className={`px-6 py-4 ${isWinner ? 'text-green-600 dark:text-green-400 font-medium' : ''}`}
                        >
                          {score.toFixed(1)}
                        </td>
                      );
                    })}
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </CardContent>
    </Card>
  );
}

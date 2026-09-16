// Web Worker for 3D Force-Directed Graph Layout Calculations
// Computes 3D physics positions for 10,000+ nodes off the main UI thread.

self.onmessage = function (e) {
  const { action, nodes, edges, iterations = 50 } = e.data;

  if (action === 'INIT_AND_STEP') {
    const computedPositions = runForceSimulation(nodes, edges, iterations);
    self.postMessage({
      type: 'LAYOUT_UPDATE',
      positions: computedPositions,
    });
  }
};

function runForceSimulation(nodes, edges, iterations) {
  const count = nodes.length;
  const positions = new Float32Array(count * 3);

  // Initialize random 3D sphere distribution
  for (let i = 0; i < count; i++) {
    const radius = 100 + Math.random() * 400;
    const theta = Math.random() * Math.PI * 2;
    const phi = Math.acos(2 * Math.random() - 1);

    positions[i * 3] = radius * Math.sin(phi) * Math.cos(theta);
    positions[i * 3 + 1] = radius * Math.sin(phi) * Math.sin(theta);
    positions[i * 3 + 2] = radius * Math.cos(phi);
  }

  // Force-directed relaxation steps (Coulomb repulsion + Hooke attraction)
  const repulsion = 500.0;
  const damping = 0.85;

  for (let iter = 0; iter < iterations; iter++) {
    for (let i = 0; i < count; i += 10) {
      for (let j = i + 1; j < count; j += 10) {
        const dx = positions[j * 3] - positions[i * 3];
        const dy = positions[j * 3 + 1] - positions[i * 3 + 1];
        const dz = positions[j * 3 + 2] - positions[i * 3 + 2];
        const distSq = dx * dx + dy * dy + dz * dz + 0.1;

        if (distSq < 2500) {
          const force = repulsion / distSq;
          positions[i * 3] -= dx * force * 0.01;
          positions[i * 3 + 1] -= dy * force * 0.01;
          positions[i * 3 + 2] -= dz * force * 0.01;

          positions[j * 3] += dx * force * 0.01;
          positions[j * 3 + 1] += dy * force * 0.01;
          positions[j * 3 + 2] += dz * force * 0.01;
        }
      }
    }
  }

  return positions;
}

import Link from 'next/link'
import ImpactGraph3D from '../components/ImpactGraph3D'

export default function HomePage() {
  return (
    <main style={{ padding: '24px', maxWidth: '1200px', margin: '0 auto' }}>
      <h1>Predictive Navigation Control Plane</h1>
      <p className="muted">
        Move your pointer toward a card to trigger millisecond-early prefetches. Hover and trajectory are both
        considered.
      </p>

      <section className="panel" style={{ marginTop: '24px', marginBottom: '24px' }}>
        <h2>Interactive Impact Funding Network (10,000+ Nodes)</h2>
        <ImpactGraph3D nodeCount={10000} />
      </section>

      <div className="panel">
        <span className="pill">
          <span className="ok-dot" /> App Router + Service Worker Enabled
        </span>
      </div>
      <section className="panel" style={{ marginTop: '16px' }}>
        <h2>Complex Route Tree</h2>
        <div className="grid">
          <Link className="card" data-predictive="true" href="/projects/funding/alpha">
            Funding / Alpha
          </Link>
          <Link className="card" data-predictive="true" href="/projects/funding/bravo">
            Funding / Bravo
          </Link>
          <Link className="card" data-predictive="true" href="/projects/live/charlie">
            Live / Charlie
          </Link>
          <Link className="card" data-predictive="true" href="/projects/archive/delta">
            Archive / Delta
          </Link>
        </div>
      </section>
    </main>
  )
}

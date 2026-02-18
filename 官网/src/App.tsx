import { BrowserRouter as Router, Routes, Route } from 'react-router-dom'
import { ThemeProvider } from '@/hooks/useTheme'
import { Header, Footer } from '@/components/layout'
import { Home, QQGroup, License } from '@/pages'

function App() {
  return (
    <ThemeProvider>
      <Router>
        <div className="min-h-screen flex flex-col bg-background text-foreground">
          <Header />
          <main className="flex-1">
            <Routes>
              <Route path="/" element={<Home />} />
              <Route path="/qqg" element={<QQGroup />} />
              <Route path="/license" element={<License />} />
            </Routes>
          </main>
          <Footer />
        </div>
      </Router>
    </ThemeProvider>
  )
}

export default App

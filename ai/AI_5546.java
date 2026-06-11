import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_5546 extends AI {
public AI_5546() throws LeekRunException {
super(3, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_s1 = ops(new SetLeekValue(AI_5546.this, new Object[] { 1l, 2l, 3l }), 7);
Object u_s2 = ops(new SetLeekValue(AI_5546.this, new Object[] { 2l, 3l, 4l }), 7);
return Set_setIntersection_hh(u_s1, u_s2);
}
protected String getAIString() { return "<snippet 5546>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 5546>", };}

protected int[] getErrorFilesID() { return new int[] {5546, };}

private SetLeekValue Set_setIntersection_hh(Object a0, Object a1) throws LeekRunException {
SetLeekValue x0; try { x0 = (SetLeekValue) (a0); } catch (ClassCastException e) { return new SetLeekValue(AI_5546.this); }
SetLeekValue x1; try { x1 = (SetLeekValue) (a1); } catch (ClassCastException e) { return new SetLeekValue(AI_5546.this); }
return x0.setIntersection(this, x1);
}

}
